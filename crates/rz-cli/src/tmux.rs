//! Thin wrapper over tmux CLI commands.
//!
//! Uses `send-keys` for short text delivery, `load-buffer`/`paste-buffer` for
//! long text, `list-panes` for structured pane discovery, and `capture-pane`
//! for reading pane output.

use std::process::{Command, Stdio};
use std::io::Write as _;

use eyre::{Result, bail};

// ---------------------------------------------------------------------------
// Pane info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PaneInfo {
    pub id: String,
    pub title: String,
    pub command: Option<String>,
    pub active: bool,
}

// ---------------------------------------------------------------------------
// Identity
// ---------------------------------------------------------------------------

/// Get own pane ID from environment (e.g. "%0").
pub fn own_pane_id() -> Result<String> {
    std::env::var("TMUX_PANE")
        .map(|id| normalize_pane_id(&id))
        .map_err(|_| eyre::eyre!("TMUX_PANE not set — not inside tmux?"))
}

/// Get the current tmux session name.
pub fn session_name() -> Result<String> {
    // Try parsing from TMUX env var (format: /tmp/tmux-1000/default,12345,0)
    if let Ok(val) = std::env::var("TMUX") {
        if let Some(path) = val.split(',').next() {
            if let Some(name) = path.rsplit('/').next() {
                if !name.is_empty() {
                    return Ok(name.to_string());
                }
            }
        }
    }
    // Fallback: ask tmux directly
    tmux_output(&["display-message", "-p", "#{session_name}"])
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Send text to a pane and submit it.
///
/// For short text (<=200 chars), uses `send-keys`. For longer text, pipes
/// through `load-buffer -` then `paste-buffer` to avoid argument length issues.
pub fn send(pane_id: &str, text: &str) -> Result<()> {
    if text.len() <= 200 {
        tmux(&["send-keys", "-t", pane_id, text, "Enter"])?;
    } else {
        // Load text into tmux buffer via stdin
        let mut child = Command::new("tmux")
            .args(["load-buffer", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!(
                "tmux load-buffer failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        // Paste buffer into target pane
        tmux(&["paste-buffer", "-t", pane_id])?;
        // Press Enter to submit
        tmux(&["send-keys", "-t", pane_id, "Enter"])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Pane lifecycle
// ---------------------------------------------------------------------------

/// Spawn a command in a new horizontal split pane. Returns the new pane ID (e.g. "%5").
pub fn spawn(cmd: &str, args: &[&str], _name: Option<&str>) -> Result<String> {
    let full_cmd = if args.is_empty() {
        cmd.to_string()
    } else {
        format!("{} {}", cmd, args.join(" "))
    };

    let output = Command::new("tmux")
        .args(["split-window", "-h", "-P", "-F", "#{pane_id}", &full_cmd])
        .output()?;
    if !output.status.success() {
        bail!(
            "tmux split-window failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Close a pane by ID.
pub fn close(pane_id: &str) -> Result<()> {
    tmux(&["kill-pane", "-t", pane_id])
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// List all panes as structured data.
pub fn list_panes() -> Result<Vec<PaneInfo>> {
    let raw = tmux_output(&[
        "list-panes", "-a", "-F",
        "#{pane_id}|#{pane_title}|#{pane_current_command}|#{pane_active}",
    ])?;
    let mut panes = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.splitn(4, '|').collect();
        if parts.len() < 4 {
            continue;
        }
        panes.push(PaneInfo {
            id: parts[0].to_string(),
            title: parts[1].to_string(),
            command: if parts[2].is_empty() {
                None
            } else {
                Some(parts[2].to_string())
            },
            active: parts[3] == "1",
        });
    }
    Ok(panes)
}

/// List pane IDs only.
pub fn list_pane_ids() -> Result<Vec<String>> {
    Ok(list_panes()?.into_iter().map(|p| p.id).collect())
}

/// Normalize a user-provided pane ID.
///
/// - `"0"` -> `"%0"`
/// - `"%0"` -> `"%0"` (passthrough)
pub fn normalize_pane_id(input: &str) -> String {
    if input.starts_with('%') {
        input.to_string()
    } else {
        format!("%{input}")
    }
}

/// Dump a pane's full scrollback.
pub fn dump(pane_id: &str) -> Result<String> {
    tmux_output(&["capture-pane", "-t", pane_id, "-p", "-S", "-"])
}

// ---------------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------------

/// Poll `dump()` until output appears, then sleep `settle_secs` for it to stabilize.
pub fn wait_for_stable_output(pane_id: &str, max_secs: u64, settle_secs: u64) {
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(max_secs);
    loop {
        if start.elapsed() >= timeout {
            break;
        }
        if let Ok(text) = dump(pane_id) {
            if !text.trim().is_empty() {
                std::thread::sleep(std::time::Duration::from_secs(settle_secs));
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

// ---------------------------------------------------------------------------
// Internal
// ---------------------------------------------------------------------------

fn tmux(args: &[&str]) -> Result<()> {
    let output = Command::new("tmux").args(args).output()?;
    if !output.status.success() {
        bail!(
            "tmux {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn tmux_output(args: &[&str]) -> Result<String> {
    let output = Command::new("tmux").args(args).output()?;
    if !output.status.success() {
        bail!(
            "tmux {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
