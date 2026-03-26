//! File-based message mailbox system.
//!
//! Each agent gets an inbox directory at `~/.rz/mailboxes/<name>/inbox/`.
//! Messages are stored as `<ts>_<id>.json` files. Writes are atomic
//! (write to `.tmp` then rename).

use eyre::{Context, Result};
use rz_agent_protocol::Envelope;
use std::fs;
use std::path::PathBuf;

/// Returns `~/.rz/mailboxes/<name>/inbox/`.
pub fn mailbox_dir(agent_name: &str) -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    home.join(".rz")
        .join("mailboxes")
        .join(agent_name)
        .join("inbox")
}

/// Create the mailbox directory tree if it doesn't exist.
pub fn ensure_mailbox(agent_name: &str) -> Result<()> {
    let dir = mailbox_dir(agent_name);
    fs::create_dir_all(&dir)
        .wrap_err_with(|| format!("failed to create mailbox dir: {}", dir.display()))?;
    Ok(())
}

/// Atomically deliver an envelope to an agent's inbox.
///
/// Writes to a `.tmp` file first, then renames into place.
pub fn deliver(agent_name: &str, envelope: &Envelope) -> Result<()> {
    let dir = mailbox_dir(agent_name);
    fs::create_dir_all(&dir)?;

    let filename = format!("{}_{}.json", envelope.ts, envelope.id);
    let final_path = dir.join(&filename);
    let tmp_path = dir.join(format!("{}.tmp", filename));

    let json = serde_json::to_string_pretty(envelope)
        .wrap_err("failed to serialize envelope")?;

    fs::write(&tmp_path, json.as_bytes())
        .wrap_err_with(|| format!("failed to write tmp file: {}", tmp_path.display()))?;

    fs::rename(&tmp_path, &final_path)
        .wrap_err_with(|| format!("failed to rename {} -> {}", tmp_path.display(), final_path.display()))?;

    Ok(())
}

/// Read all pending messages sorted by filename (oldest first), deleting each after read.
pub fn receive(agent_name: &str) -> Result<Vec<Envelope>> {
    let dir = mailbox_dir(agent_name);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .wrap_err("failed to read mailbox dir")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect();

    entries.sort_by_key(|e| e.file_name());

    let mut envelopes = Vec::with_capacity(entries.len());
    for entry in entries {
        let path = entry.path();
        let data = fs::read_to_string(&path)
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;
        let env: Envelope = serde_json::from_str(&data)
            .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
        fs::remove_file(&path)?;
        envelopes.push(env);
    }

    Ok(envelopes)
}

/// Pop the oldest pending message, deleting it after read.
pub fn receive_one(agent_name: &str) -> Result<Option<Envelope>> {
    let dir = mailbox_dir(agent_name);
    if !dir.exists() {
        return Ok(None);
    }

    let mut entries: Vec<_> = fs::read_dir(&dir)
        .wrap_err("failed to read mailbox dir")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect();

    if entries.is_empty() {
        return Ok(None);
    }

    entries.sort_by_key(|e| e.file_name());

    let path = entries[0].path();
    let data = fs::read_to_string(&path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;
    let env: Envelope = serde_json::from_str(&data)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;
    fs::remove_file(&path)?;

    Ok(Some(env))
}

/// Count pending messages without consuming them.
pub fn count(agent_name: &str) -> Result<usize> {
    let dir = mailbox_dir(agent_name);
    if !dir.exists() {
        return Ok(0);
    }

    let n = fs::read_dir(&dir)
        .wrap_err("failed to read mailbox dir")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .count();

    Ok(n)
}

/// Delete all pending messages.
pub fn clear(agent_name: &str) -> Result<()> {
    let dir = mailbox_dir(agent_name);
    if !dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().map(|ext| ext == "json").unwrap_or(false) {
            fs::remove_file(&path)?;
        }
    }

    Ok(())
}
