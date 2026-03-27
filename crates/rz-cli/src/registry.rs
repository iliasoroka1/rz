//! Agent registry for discovery and routing.
//!
//! Persists agent entries to `~/.rz/registry.json` so any process
//! can discover peers by name, transport, or capability.

use eyre::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single registered agent.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AgentEntry {
    pub name: String,
    /// UUID or cmux surface ID.
    pub id: String,
    /// One of: `cmux`, `http`, `file`, `nats`.
    pub transport: String,
    /// Surface ID for cmux, URL for http, agent name for nats.
    pub endpoint: String,
    /// Optional tags like `["code","review","search"]`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// If true, entry persists in registry after agent exits.
    #[serde(default)]
    pub permanent: bool,
    /// Unix epoch milliseconds when the agent first registered.
    pub registered_at: u64,
    /// Unix epoch milliseconds, updated by heartbeat.
    pub last_seen: u64,
}

/// Max inactivity before a non-permanent agent is pruned (10 minutes).
const STALE_THRESHOLD_MS: u64 = 10 * 60 * 1000;

/// Return the path to the registry file (`~/.rz/registry.json`).
pub fn registry_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".rz").join("registry.json")
}

/// Load the registry from disk. Returns an empty map if the file does not exist.
pub fn load() -> Result<HashMap<String, AgentEntry>> {
    let path = registry_path();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = fs::read_to_string(&path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let map: HashMap<String, AgentEntry> =
        serde_json::from_str(&data).wrap_err("failed to parse registry.json")?;
    Ok(map)
}

/// Atomically write the registry to disk (write-tmp then rename).
pub fn save(registry: &HashMap<String, AgentEntry>) -> Result<()> {
    let path = registry_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(registry)
        .wrap_err("failed to serialize registry")?;

    // Atomic write: temp file in same dir, then rename.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json.as_bytes())
        .wrap_err_with(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, &path)
        .wrap_err_with(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Register (or update) an agent entry.
pub fn register(entry: AgentEntry) -> Result<()> {
    let mut reg = load()?;
    reg.insert(entry.name.clone(), entry);
    save(&reg)
}

/// Remove an agent by name.
pub fn deregister(name: &str) -> Result<()> {
    let mut reg = load()?;
    reg.remove(name);
    save(&reg)
}

/// Look up an agent by name.
pub fn lookup(name: &str) -> Result<Option<AgentEntry>> {
    let reg = load()?;
    Ok(reg.get(name).cloned())
}

/// Return all registered agents.
pub fn list_all() -> Result<Vec<AgentEntry>> {
    let reg = load()?;
    Ok(reg.into_values().collect())
}

/// Remove entries whose `last_seen` is older than `max_age_secs` seconds ago.
/// Returns the number of entries removed.
pub fn cleanup_stale(max_age_secs: u64) -> Result<usize> {
    let mut reg = load()?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff = now_ms.saturating_sub(max_age_secs * 1000);

    let before = reg.len();
    reg.retain(|_, entry| entry.last_seen >= cutoff);
    let removed = before - reg.len();

    if removed > 0 {
        save(&reg)?;
    }
    Ok(removed)
}

/// Update `last_seen` to now for the given agent name.
pub fn touch(name: &str) -> Result<()> {
    let mut reg = load()?;
    if let Some(entry) = reg.get_mut(name) {
        entry.last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        save(&reg)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// NATS KV-backed registry
// ---------------------------------------------------------------------------

const NATS_KV_BUCKET: &str = "rz-agents";

/// Get or create the NATS KV store for agent registration.
async fn nats_kv_store(
    js: &async_nats::jetstream::Context,
) -> Result<async_nats::jetstream::kv::Store> {
    match js.get_key_value(NATS_KV_BUCKET).await {
        Ok(store) => Ok(store),
        Err(_) => {
            let store = js
                .create_key_value(async_nats::jetstream::kv::Config {
                    bucket: NATS_KV_BUCKET.to_string(),
                    max_age: std::time::Duration::ZERO, // no auto-expiry — managed by heartbeat + pruning
                    ..Default::default()
                })
                .await
                .map_err(|e| eyre::eyre!("NATS KV create bucket failed: {e}"))?;
            Ok(store)
        }
    }
}

/// Register an agent in the NATS KV bucket `rz-agents`.
/// If `RZ_HUB` is not set, returns `Ok(())` silently.
pub fn nats_register(entry: &AgentEntry) -> Result<()> {
    let url = match crate::nats_hub::hub_url() {
        Some(u) => u,
        None => return Ok(()),
    };
    let json = serde_json::to_string(entry)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;
        let js = async_nats::jetstream::new(client);
        let store = nats_kv_store(&js).await?;
        store
            .put(&entry.name, json.into_bytes().into())
            .await
            .map_err(|e| eyre::eyre!("NATS KV put failed: {e}"))?;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}

/// Remove an agent from the NATS KV bucket `rz-agents`.
/// If `RZ_HUB` is not set, returns `Ok(())` silently.
pub fn nats_deregister(name: &str) -> Result<()> {
    let url = match crate::nats_hub::hub_url() {
        Some(u) => u,
        None => return Ok(()),
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;
        let js = async_nats::jetstream::new(client);
        let store = nats_kv_store(&js).await?;
        // Delete returns an error if key doesn't exist; ignore it.
        let _ = store.delete(name).await;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}

/// List all agents from the NATS KV bucket `rz-agents`.
/// If `RZ_HUB` is not set or bucket doesn't exist, returns an empty vec.
pub fn nats_list() -> Result<Vec<AgentEntry>> {
    let url = match crate::nats_hub::hub_url() {
        Some(u) => u,
        None => return Ok(Vec::new()),
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;
        let js = async_nats::jetstream::new(client);
        let store = match js.get_key_value(NATS_KV_BUCKET).await {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()), // bucket doesn't exist
        };

        use futures::StreamExt;
        let mut keys = match store.keys().await {
            Ok(k) => k,
            Err(_) => return Ok(Vec::new()),
        };

        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut entries = Vec::new();
        let mut to_prune = Vec::new();
        while let Some(key) = keys.next().await {
            let key = match key {
                Ok(k) => k,
                Err(_) => continue,
            };
            if let Ok(Some(kv_entry)) = store.get(&key).await {
                if let Ok(agent) =
                    serde_json::from_slice::<AgentEntry>(&kv_entry)
                {
                    // Prune stale non-permanent agents.
                    if !agent.permanent && now_ms.saturating_sub(agent.last_seen) > STALE_THRESHOLD_MS {
                        to_prune.push(agent.name.clone());
                        continue;
                    }
                    entries.push(agent);
                }
            }
        }
        // Clean up stale entries.
        for name in &to_prune {
            let _ = store.delete(name).await;
        }
        Ok(entries)
    })
}

/// Look up a single agent by name from the NATS KV bucket.
/// If `RZ_HUB` is not set or key not found, returns `None`.
pub fn nats_lookup(name: &str) -> Result<Option<AgentEntry>> {
    let url = match crate::nats_hub::hub_url() {
        Some(u) => u,
        None => return Ok(None),
    };

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;
        let js = async_nats::jetstream::new(client);
        let store = match js.get_key_value(NATS_KV_BUCKET).await {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        match store.get(name).await {
            Ok(Some(value)) => {
                let agent = serde_json::from_slice::<AgentEntry>(&value)
                    .map_err(|e| eyre::eyre!("NATS KV deserialize failed: {e}"))?;
                // Prune stale non-permanent agents.
                if !agent.permanent {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    if now_ms.saturating_sub(agent.last_seen) > STALE_THRESHOLD_MS {
                        let _ = store.delete(name).await;
                        return Ok(None);
                    }
                }
                Ok(Some(agent))
            }
            Ok(None) | Err(_) => Ok(None),
        }
    })
}

/// Re-put an agent entry with updated `last_seen` to keep it alive.
/// If `RZ_HUB` is not set, returns `Ok(())` silently.
pub fn nats_heartbeat(name: &str, entry: &AgentEntry) -> Result<()> {
    let url = match crate::nats_hub::hub_url() {
        Some(u) => u,
        None => return Ok(()),
    };
    let mut refreshed = entry.clone();
    refreshed.last_seen = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let json = serde_json::to_string(&refreshed)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;
        let js = async_nats::jetstream::new(client);
        let store = nats_kv_store(&js).await?;
        store
            .put(name, json.into_bytes().into())
            .await
            .map_err(|e| eyre::eyre!("NATS KV heartbeat put failed: {e}"))?;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}
