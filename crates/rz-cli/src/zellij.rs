//! Thin wrapper over Zellij CLI commands.
//!
//! Uses `paste` for message delivery (with a short delay + `write 13` to
//! trigger submission), `list-panes --json` for structured pane discovery,
//! and `dump-screen` for reading pane output.

use std::process::Command;

use eyre::{Result, bail};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Pane info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct PaneInfo {
    pub id: u64,
    pub is_plugin: bool,
    pub is_focused: bool,
    pub is_floating: bool,
    pub title: String,
    #[serde(default)]
    pub exited: bool,
    #[serde(default)]
    pub exit_status: Option<i32>,
    #[serde(default)]
    pub pane_command: Option<String>,
    #[serde(default)]
    pub pane_cwd: Option<String>,
    #[serde(default)]
    pub tab_id: Option<u64>,
    #[serde(default)]
    pub tab_name: Option<String>,
}

impl PaneInfo {
    /// Full pane ID string (e.g. "terminal_3" or "plugin_1").
    pub fn pane_id(&self) -> String {
        let prefix = if self.is_plugin { "plugin" } else { "terminal" };
        format!("{prefix}_{}", self.id)
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

/// Send text to a pane and submit it.
///
/// Uses `paste` (bracketed paste mode) for the content, a short delay for
/// the terminal to process the paste, then `write 13` (CR byte) to submit.
pub fn send(pane_id: &str, text: &str) -> Result<()> {
    zellij(&["action", "paste", "--pane-id", pane_id, text])?;
    std::thread::sleep(std::time::Duration::from_millis(200));
    zellij(&["action", "write", "--pane-id", pane_id, "13"])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Pane lifecycle
// ---------------------------------------------------------------------------

/// Spawn a command in a new pane. Returns the pane ID (e.g. "terminal_3").
pub fn spawn(cmd: &str, args: &[&str], name: Option<&str>) -> Result<String> {
    let mut cli_args = vec!["run"];
    if let Some(n) = name {
        cli_args.extend(["--name", n]);
    }
    cli_args.push("--");
    cli_args.push(cmd);
    cli_args.extend(args);

    let output = Command::new("zellij").args(&cli_args).output()?;
    if !output.status.success() {
        bail!(
            "zellij run failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Close a pane by ID.
pub fn close(pane_id: &str) -> Result<()> {
    zellij(&["action", "close-pane", "--pane-id", pane_id])
}

/// Rename a pane (appears on the pane frame).
pub fn rename(pane_id: &str, name: &str) -> Result<()> {
    zellij(&["action", "rename-pane", "--pane-id", pane_id, name])
}

/// Set a pane's foreground and/or background color.
pub fn set_color(pane_id: &str, fg: Option<&str>, bg: Option<&str>) -> Result<()> {
    let mut args = vec!["action", "set-pane-color", "--pane-id", pane_id];
    if let Some(f) = fg {
        args.extend(["--fg", f]);
    }
    if let Some(b) = bg {
        args.extend(["--bg", b]);
    }
    zellij(&args)
}

/// Reset a pane's colors to terminal defaults.
pub fn reset_color(pane_id: &str) -> Result<()> {
    zellij(&["action", "set-pane-color", "--pane-id", pane_id, "--reset"])
}

// ---------------------------------------------------------------------------
// Query
// ---------------------------------------------------------------------------

/// List all panes as structured data.
pub fn list_panes() -> Result<Vec<PaneInfo>> {
    let output = Command::new("zellij")
        .args(["action", "list-panes", "--json"])
        .output()?;
    if !output.status.success() {
        bail!(
            "list-panes failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// List terminal pane IDs only (excludes plugins).
pub fn list_pane_ids() -> Result<Vec<String>> {
    Ok(list_panes()?
        .into_iter()
        .filter(|p| !p.is_plugin)
        .map(|p| p.pane_id())
        .collect())
}

/// Normalize a user-provided pane ID.
///
/// - `"3"` → `"terminal_3"`
/// - `"terminal_3"` → `"terminal_3"` (passthrough)
/// - `"plugin_1"` → `"plugin_1"` (passthrough)
pub fn normalize_pane_id(input: &str) -> String {
    if input.starts_with("terminal_") || input.starts_with("plugin_") {
        input.to_string()
    } else if input.chars().all(|c| c.is_ascii_digit()) {
        format!("terminal_{input}")
    } else {
        input.to_string()
    }
}

/// Dump a pane's full scrollback.
pub fn dump(pane_id: &str) -> Result<String> {
    let output = Command::new("zellij")
        .args(["action", "dump-screen", "--pane-id", pane_id, "--full"])
        .output()?;
    if !output.status.success() {
        bail!(
            "dump-screen failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Dump a pane's scrollback, returning only the last `n` lines.
pub fn dump_last(pane_id: &str, lines: usize) -> Result<String> {
    let full = dump(pane_id)?;
    let taken: Vec<&str> = full.lines().rev().take(lines).collect();
    let mut taken = taken;
    taken.reverse();
    Ok(taken.join("\n"))
}

/// Get own pane ID from environment.
pub fn own_pane_id() -> Result<String> {
    std::env::var("ZELLIJ_PANE_ID")
        .map(|id| normalize_pane_id(&id))
        .map_err(|_| eyre::eyre!("ZELLIJ_PANE_ID not set — not inside zellij?"))
}

// ---------------------------------------------------------------------------
// Hub (plugin pipe)
// ---------------------------------------------------------------------------

/// Send a pipe message to the rz-hub plugin and return its JSON response.
///
/// Uses `--name rz` without `--plugin` to target already-running instances
/// (using `--plugin` launches a new instance each time).
/// Percent-encode characters that conflict with the comma-separated key=value arg format.
fn encode_arg_value(v: &str) -> String {
    v.replace('%', "%25").replace(',', "%2C").replace('=', "%3D")
}

pub fn pipe_to_hub(action: &str, args: &[(&str, &str)], payload: Option<&str>) -> Result<String> {
    let mut parts = vec![action.to_string()];
    for (k, v) in args {
        parts.push(format!("{k}={}", encode_arg_value(v)));
    }
    let args_str = parts.join(",");

    let mut cli_args = vec![
        "pipe",
        "--name", "rz",
        "--args", &args_str,
    ];
    if let Some(p) = payload {
        cli_args.push("--");
        cli_args.push(p);
    }

    let output = Command::new("zellij").args(&cli_args).output()?;
    if !output.status.success() {
        bail!(
            "zellij pipe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Check if hub routing is available.
///
/// Priority: `RZ_HUB=1` forces on, `RZ_HUB=0` forces off.  Otherwise we
/// auto-detect by sending a quick `action=ping` pipe to the hub plugin.  The
/// result is cached for the lifetime of the process so the probe runs at most
/// once.
pub fn hub_available() -> bool {
    use std::sync::OnceLock;

    // Explicit override takes precedence.
    if let Ok(v) = std::env::var("RZ_HUB") {
        return v == "1";
    }

    // Must be inside Zellij.
    if std::env::var("ZELLIJ").is_err() {
        return false;
    }

    static RESULT: OnceLock<bool> = OnceLock::new();
    *RESULT.get_or_init(probe_hub)
}

/// Try a lightweight ping to the hub plugin with a 2-second timeout.
///
/// If the plugin is loaded (via `load_plugins`), the response is near-instant.
/// If it isn't loaded, `zellij pipe` may block waiting for a subscriber — the
/// timeout catches that case.
fn probe_hub() -> bool {
    let mut child = match Command::new("zellij")
        .args(["pipe", "--name", "rz", "--args", "action=ping"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(_) => return false,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal
// ---------------------------------------------------------------------------

fn zellij(args: &[&str]) -> Result<()> {
    let output = Command::new("zellij").args(args).output()?;
    if !output.status.success() {
        bail!(
            "zellij {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
