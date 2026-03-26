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
    /// One of: `cmux`, `http`, `file`, `stdio`.
    pub transport: String,
    /// Surface ID for cmux, URL for http, mailbox path for file.
    pub endpoint: String,
    /// Optional tags like `["code","review","search"]`.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Unix epoch milliseconds when the agent first registered.
    pub registered_at: u64,
    /// Unix epoch milliseconds, updated by [`touch`].
    pub last_seen: u64,
}

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
