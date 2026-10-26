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
    /// Hostname of the machine the agent runs on. Lets resolvers pid-check
    /// `pty-<pid>`/`listen-<pid>` entries only when they are local.
    /// Empty on entries written by older versions (treated as local).
    #[serde(default)]
    pub host: String,
}

/// This machine's hostname (cached). Empty string if it can't be determined.
pub fn local_host() -> String {
    use std::sync::OnceLock;
    static HOST: OnceLock<String> = OnceLock::new();
    HOST.get_or_init(|| {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    })
    .clone()
}

/// Return `true` if the entry was registered from this machine (or predates
/// the `host` field, in which case we assume local).
pub fn is_local(entry: &AgentEntry) -> bool {
    entry.host.is_empty() || entry.host == local_host()
}

/// Max inactivity before a non-permanent agent is pruned (10 minutes).
const STALE_THRESHOLD_MS: u64 = 10 * 60 * 1000;

/// Return `true` if a non-permanent entry hasn't been seen within the stale
/// threshold. Permanent entries are never stale.
pub fn is_stale(entry: &AgentEntry, now_ms: u64) -> bool {
    !entry.permanent && now_ms.saturating_sub(entry.last_seen) > STALE_THRESHOLD_MS
}

/// Return the path to the registry file (`~/.rz/registry.json`).
pub fn registry_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".rz").join("registry.json")
}

/// Load the registry from disk. Returns an empty map if the file does not exist.
pub fn load() -> Result<HashMap<String, AgentEntry>> {
    load_from(&registry_path())
}

fn load_from(path: &std::path::Path) -> Result<HashMap<String, AgentEntry>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let map: HashMap<String, AgentEntry> =
        serde_json::from_str(&data).wrap_err("failed to parse registry.json")?;
    Ok(map)
}

/// Atomically write the registry to disk (write-tmp then rename).
pub fn save(registry: &HashMap<String, AgentEntry>) -> Result<()> {
    save_to(&registry_path(), registry)
}

fn save_to(path: &std::path::Path, registry: &HashMap<String, AgentEntry>) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(registry)
        .wrap_err("failed to serialize registry")?;

    // Atomic write: temp file in same dir, then rename. The temp name is per
    // process so concurrent writers never rename each other's file away.
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    fs::write(&tmp, json.as_bytes())
        .wrap_err_with(|| format!("failed to write {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .wrap_err_with(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Exclusive lock on `~/.rz/registry.lock`, released when dropped. Every
/// load-modify-save holds it: listeners started together used to race, and
/// all but one exited on a failed rename (or silently dropped an entry).
struct RegistryLock {
    _file: fs::File,
}

impl RegistryLock {
    fn acquire(registry: &std::path::Path) -> Result<Self> {
        use std::os::fd::AsRawFd;
        let path = registry.with_extension("lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .wrap_err_with(|| format!("failed to create {}", parent.display()))?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .wrap_err_with(|| format!("failed to open {}", path.display()))?;
        // SAFETY: flock on a valid, owned descriptor; blocks until acquired.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error())
                .wrap_err_with(|| format!("failed to lock {}", path.display()));
        }
        Ok(Self { _file: file })
    }
}

/// Load, modify and save the registry under the lock. `f` returns whether it
/// changed anything (nothing is written otherwise) plus a result to pass back.
fn update<T>(f: impl FnOnce(&mut HashMap<String, AgentEntry>) -> (bool, T)) -> Result<T> {
    update_at(&registry_path(), f)
}

fn update_at<T>(
    path: &std::path::Path,
    f: impl FnOnce(&mut HashMap<String, AgentEntry>) -> (bool, T),
) -> Result<T> {
    let _lock = RegistryLock::acquire(path)?;
    let mut reg = load_from(path)?;
    let (changed, out) = f(&mut reg);
    if changed {
        save_to(path, &reg)?;
    }
    Ok(out)
}

/// Register (or update) an agent entry. Keys are session-scoped so
/// identically-named agents in different sessions coexist in one file.
pub fn register(entry: AgentEntry) -> Result<()> {
    update(|reg| {
        reg.insert(crate::session::scoped_key(&entry.name), entry);
        (true, ())
    })
}

/// Remove an agent by name (current session only).
pub fn deregister(name: &str) -> Result<()> {
    update(|reg| (reg.remove(&crate::session::scoped_key(name)).is_some(), ()))
}

/// Look up an agent by name (current session only).
pub fn lookup(name: &str) -> Result<Option<AgentEntry>> {
    let reg = load()?;
    Ok(reg.get(&crate::session::scoped_key(name)).cloned())
}

/// Return all agents registered in the current session.
pub fn list_all() -> Result<Vec<AgentEntry>> {
    let reg = load()?;
    Ok(reg
        .into_iter()
        .filter(|(k, _)| crate::session::unscope_key(k).is_some())
        .map(|(_, v)| v)
        .collect())
}

/// Remove entries whose `last_seen` is older than `max_age_secs` seconds ago.
/// Returns the number of entries removed.
pub fn cleanup_stale(max_age_secs: u64) -> Result<usize> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff = now_ms.saturating_sub(max_age_secs * 1000);
    update(|reg| {
        let before = reg.len();
        reg.retain(|_, entry| entry.last_seen >= cutoff);
        let removed = before - reg.len();
        (removed > 0, removed)
    })
}

/// Update `last_seen` to now for the given agent name (current session).
pub fn touch(name: &str) -> Result<()> {
    update(|reg| match reg.get_mut(&crate::session::scoped_key(name)) {
        Some(entry) => {
            entry.last_seen = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            (true, ())
        }
        None => (false, ()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_updates_keep_every_entry() {
        let dir = std::env::temp_dir().join(format!("rz-registry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("registry.json");
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    update_at(&path, |reg| {
                        let mut e = entry(false, i);
                        e.name = format!("agent{i}");
                        reg.insert(e.name.clone(), e);
                        (true, ())
                    })
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap().unwrap();
        }
        assert_eq!(load_from(&path).unwrap().len(), 16);
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    fn entry(permanent: bool, last_seen: u64) -> AgentEntry {
        AgentEntry {
            name: "t".into(),
            id: "listen-1".into(),
            transport: "nats".into(),
            endpoint: "t".into(),
            capabilities: vec![],
            permanent,
            registered_at: last_seen,
            host: String::new(),
            last_seen,
        }
    }

    #[test]
    fn fresh_entry_is_not_stale() {
        let now = 10 * 60 * 1000;
        assert!(!is_stale(&entry(false, now - 1000), now));
        // Exactly at the threshold is still fresh.
        assert!(!is_stale(&entry(false, 0), STALE_THRESHOLD_MS));
    }

    #[test]
    fn old_entry_is_stale_unless_permanent() {
        let now = STALE_THRESHOLD_MS + 1;
        assert!(is_stale(&entry(false, 0), now));
        assert!(!is_stale(&entry(true, 0), now));
    }

    #[test]
    fn clock_skew_does_not_underflow() {
        // last_seen in the future (clock skew) must not panic or mark stale.
        assert!(!is_stale(&entry(false, u64::MAX), 0));
    }
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
    if crate::nats_hub::hub_url().is_none() {
        return Ok(());
    }
    let json = serde_json::to_string(entry)?;
    let client = crate::nats_hub::client()?;

    crate::nats_hub::runtime().block_on(async {
        let js = async_nats::jetstream::new(client);
        let store = nats_kv_store(&js).await?;
        store
            .put(
                &crate::session::scoped_key(&entry.name),
                json.into_bytes().into(),
            )
            .await
            .map_err(|e| eyre::eyre!("NATS KV put failed: {e}"))?;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}

/// Remove an agent from the NATS KV bucket `rz-agents`.
/// If `RZ_HUB` is not set, returns `Ok(())` silently.
pub fn nats_deregister(name: &str) -> Result<()> {
    if crate::nats_hub::hub_url().is_none() {
        return Ok(());
    }
    let client = crate::nats_hub::client()?;

    crate::nats_hub::runtime().block_on(async {
        let js = async_nats::jetstream::new(client);
        let store = nats_kv_store(&js).await?;
        // Delete returns an error if key doesn't exist; ignore it.
        let _ = store.delete(&crate::session::scoped_key(name)).await;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}

/// List all agents from the NATS KV bucket `rz-agents`.
/// If `RZ_HUB` is not set or bucket doesn't exist, returns an empty vec.
pub fn nats_list() -> Result<Vec<AgentEntry>> {
    if crate::nats_hub::hub_url().is_none() {
        return Ok(Vec::new());
    }
    let client = crate::nats_hub::client()?;

    crate::nats_hub::runtime().block_on(async {
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
            // Only surface agents from our own session.
            if crate::session::unscope_key(&key).is_none() {
                continue;
            }
            if let Ok(Some(kv_entry)) = store.get(&key).await {
                if let Ok(agent) =
                    serde_json::from_slice::<AgentEntry>(&kv_entry)
                {
                    // Prune stale non-permanent agents.
                    if is_stale(&agent, now_ms) {
                        to_prune.push(key);
                        continue;
                    }
                    entries.push(agent);
                }
            }
        }
        // Clean up stale entries.
        for key in &to_prune {
            let _ = store.delete(key).await;
        }
        Ok(entries)
    })
}

/// Look up a single agent by name from the NATS KV bucket.
/// If `RZ_HUB` is not set or key not found, returns `None`.
pub fn nats_lookup(name: &str) -> Result<Option<AgentEntry>> {
    if crate::nats_hub::hub_url().is_none() {
        return Ok(None);
    }
    let client = crate::nats_hub::client()?;

    crate::nats_hub::runtime().block_on(async {
        let js = async_nats::jetstream::new(client);
        let store = match js.get_key_value(NATS_KV_BUCKET).await {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let key = crate::session::scoped_key(name);
        match store.get(&key).await {
            Ok(Some(value)) => {
                let agent = serde_json::from_slice::<AgentEntry>(&value)
                    .map_err(|e| eyre::eyre!("NATS KV deserialize failed: {e}"))?;
                // Prune stale non-permanent agents.
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if is_stale(&agent, now_ms) {
                    let _ = store.delete(&key).await;
                    return Ok(None);
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
    if crate::nats_hub::hub_url().is_none() {
        return Ok(());
    }
    let mut refreshed = entry.clone();
    refreshed.last_seen = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let json = serde_json::to_string(&refreshed)?;
    let client = crate::nats_hub::client()?;

    crate::nats_hub::runtime().block_on(async {
        let js = async_nats::jetstream::new(client);
        let store = nats_kv_store(&js).await?;
        store
            .put(&crate::session::scoped_key(name), json.into_bytes().into())
            .await
            .map_err(|e| eyre::eyre!("NATS KV heartbeat put failed: {e}"))?;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}
