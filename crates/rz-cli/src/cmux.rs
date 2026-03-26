use eyre::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

/// Information about a cmux surface (analogous to a Zellij pane).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfaceInfo {
    pub id: String,
    pub title: String,
    pub workspace_id: String,
    pub workspace_name: Option<String>,
    pub is_focused: bool,
    pub surface_type: String,
}

/// Generate a simple UUID v4-style random ID for JSON-RPC requests.
fn generate_request_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let s = RandomState::new();
    let mut h = s.build_hasher();
    h.write_u64(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64);
    let a = h.finish();
    let mut h2 = s.build_hasher();
    h2.write_u64(a.wrapping_mul(6364136223846793005));
    let b = h2.finish();
    format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        (a >> 32) as u32,
        (a >> 16) as u16 & 0xffff,
        a as u16 & 0x0fff,
        (b >> 48) as u16 & 0x3fff | 0x8000,
        b & 0xffffffffffff
    )
}

/// Resolve the cmux socket path.
pub fn socket_path() -> Result<String> {
    if let Ok(path) = std::env::var("CMUX_SOCKET_PATH") {
        return Ok(path);
    }
    let home = std::env::var("HOME").wrap_err("HOME not set")?;
    Ok(format!("{}/.local/share/cmux/cmux.sock", home))
}

/// Connect to the cmux socket and make a JSON-RPC v2 call.
/// Each call creates a fresh connection (no pooling).
fn v2_call(method: &str, params: Value) -> Result<Value> {
    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .wrap_err_with(|| format!("failed to connect to cmux socket at {}", path))?;

    let id = generate_request_id();
    let request = json!({
        "id": id,
        "method": method,
        "params": params,
    });

    let mut payload = serde_json::to_string(&request)?;
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .wrap_err("failed to write to cmux socket")?;
    stream.flush()?;

    // Set a read timeout so we don't hang forever.
    stream.set_read_timeout(Some(std::time::Duration::from_secs(15)))?;

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .wrap_err("failed to read response from cmux socket")?;

    if line.is_empty() {
        bail!("empty response from cmux socket");
    }

    let trimmed = line.trim();

    // cmux may return plain-text errors before JSON (e.g. "ERROR: Access denied ...")
    if trimmed.starts_with("ERROR:") {
        bail!("{}", trimmed);
    }

    let resp: Value = serde_json::from_str(trimmed)
        .wrap_err("failed to parse cmux response")?;

    // Check response ID matches
    if resp.get("id").and_then(|v| v.as_str()) != Some(&id) {
        bail!("response id mismatch");
    }

    if resp.get("ok") == Some(&json!(false)) {
        let err = resp
            .get("error")
            .cloned()
            .unwrap_or_else(|| json!({"message": "unknown error"}));
        let msg = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        let msg = if msg == "A JavaScript exception occurred" {
            if method.contains("eval") {
                "JavaScript error (check script syntax or that surface is still loaded)".to_string()
            } else if method.contains("click") || method.contains("fill") {
                "Element not found or surface not ready".to_string()
            } else {
                msg.to_string()
            }
        } else {
            msg.to_string()
        };
        bail!("cmux error [{}]: {}", method, msg);
    }

    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

/// Send text to a surface (terminal pane) and submit with Enter.
///
/// A short delay between paste and Enter is critical: TUI apps like
/// Claude Code need time to process pasted text before the Enter key
/// can trigger submission.
pub fn send(surface_id: &str, text: &str) -> Result<()> {
    v2_call("surface.send_text", json!({
        "surface_id": surface_id,
        "text": text,
    }))?;
    // Let the TUI process the pasted text before pressing Enter.
    std::thread::sleep(std::time::Duration::from_millis(200));
    // surface.send_text pastes but doesn't submit — follow with Enter
    v2_call("surface.send_key", json!({
        "surface_id": surface_id,
        "key": "enter",
    }))?;
    Ok(())
}

/// Spawn a new terminal surface in the current workspace, run a command in it.
///
/// Two-phase startup:
///   Phase 1 — wait for the shell prompt (surface ready)
///   Phase 2 — type and submit the command
///
/// The caller is responsible for a third phase: waiting for the command's
/// own interactive prompt before sending further input (e.g. bootstrap).
///
/// Returns the new surface's ID.
pub fn spawn(cmd: &str, args: &[&str], name: Option<&str>) -> Result<String> {
    let workspace_id = std::env::var("CMUX_WORKSPACE_ID")
        .wrap_err("CMUX_WORKSPACE_ID not set — are you running inside cmux?")?;

    let mut params = json!({
        "workspace_id": workspace_id,
        "direction": "right",
    });
    if let Some(n) = name {
        params["title"] = json!(n);
    }

    let result = v2_call("surface.split", params)?;
    let surface_id = result
        .get("surface_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| eyre::eyre!("surface.split did not return surface_id"))?;

    if !cmd.is_empty() {
        // Phase 1: wait up to 15s for shell to appear, then settle 7s.
        wait_for_stable_output(&surface_id, 15, 7);

        // Type and submit the command.
        let mut full_cmd = shell_escape_arg(cmd);
        for arg in args {
            full_cmd.push(' ');
            full_cmd.push_str(&shell_escape_arg(arg));
        }

        v2_call("surface.send_text", json!({
            "surface_id": surface_id,
            "text": full_cmd,
        }))?;
        v2_call("surface.send_key", json!({
            "surface_id": surface_id,
            "key": "enter",
        }))?;
    }

    Ok(surface_id)
}

/// Wait until a surface has output, then wait a fixed settle time.
///
/// Two-step: first poll until any output appears (the process has started),
/// then sleep `settle_secs` to let the process finish its loading sequence
/// and reach its interactive prompt.
///
/// `max_secs`    — give up and proceed after this many seconds regardless
/// `settle_secs` — fixed pause after output first appears
///
/// Always returns — callers should proceed regardless.
pub fn wait_for_stable_output(surface_id: &str, max_secs: u64, settle_secs: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(max_secs);
    let poll = std::time::Duration::from_millis(300);

    // Step 1: wait until any output appears
    loop {
        if std::time::Instant::now() >= deadline {
            return; // nothing appeared — proceed anyway
        }
        let text = read_text(surface_id).unwrap_or_default();
        if !text.trim().is_empty() {
            break;
        }
        std::thread::sleep(poll);
    }

    // Step 2: fixed settle time, capped at remaining budget
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let settle = std::time::Duration::from_secs(settle_secs).min(remaining);
    std::thread::sleep(settle);
}

fn shell_escape_arg(s: &str) -> String {
    // Single-quote wrap with internal single-quote escaping
    if s.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '=')) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Close a surface.
pub fn close(surface_id: &str) -> Result<()> {
    v2_call(
        "surface.close",
        json!({ "surface_id": surface_id }),
    )?;
    Ok(())
}

/// List all surfaces with full info.
pub fn list_surfaces() -> Result<Vec<SurfaceInfo>> {
    let result = v2_call("surface.list", json!({}))?;

    // Response is { surfaces: [...], workspace_id: ..., ... }
    let workspace_id = result
        .get("workspace_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let surfaces = result
        .get("surfaces")
        .and_then(|v| v.as_array())
        .ok_or_else(|| eyre::eyre!("surface.list did not return surfaces array"))?;

    let mut out = Vec::with_capacity(surfaces.len());
    for s in surfaces {
        out.push(SurfaceInfo {
            id: s.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            title: s.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            workspace_id: workspace_id.clone(),
            workspace_name: None,
            is_focused: s.get("focused").and_then(|v| v.as_bool()).unwrap_or(false),
            // API uses "type" not "surface_type"
            surface_type: s.get("type").and_then(|v| v.as_str()).unwrap_or("terminal").to_string(),
        });
    }
    Ok(out)
}

/// List IDs of terminal surfaces only.
pub fn list_surface_ids() -> Result<Vec<String>> {
    let surfaces = list_surfaces()?;
    Ok(surfaces
        .into_iter()
        .filter(|s| s.surface_type == "terminal")
        .map(|s| s.id)
        .collect())
}

/// Read text content from a surface's terminal.
pub fn read_text(surface_id: &str) -> Result<String> {
    let result = v2_call(
        "surface.read_text",
        json!({ "surface_id": surface_id }),
    )?;

    // API returns { base64: "..." } — decode it
    if let Some(b64) = result.get("base64").and_then(|v| v.as_str()) {
        return base64_decode_str(b64);
    }
    // Fallback: plain text field or raw string
    result
        .get("text").and_then(|v| v.as_str()).map(|s| s.to_string())
        .or_else(|| result.as_str().map(|s| s.to_string()))
        .ok_or_else(|| eyre::eyre!("surface.read_text did not return text or base64"))
}

fn base64_decode_str(input: &str) -> Result<String> {
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut buf = Vec::with_capacity(input.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for &byte in input.as_bytes() {
        if byte == b'=' || byte == b'\n' || byte == b'\r' || byte == b' ' { continue; }
        let val = table.iter().position(|&b| b == byte)
            .ok_or_else(|| eyre::eyre!("invalid base64 character: {}", byte as char))? as u32;
        acc = (acc << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    String::from_utf8(buf).wrap_err("surface text is not valid UTF-8")
}

/// Get this surface's own ID from the environment.
pub fn own_surface_id() -> Result<String> {
    std::env::var("CMUX_SURFACE_ID")
        .wrap_err("CMUX_SURFACE_ID not set — are you running inside cmux?")
}

/// Create a notification.
pub fn notify(title: &str, body: Option<&str>, surface_id: Option<&str>) -> Result<()> {
    let mut params = json!({ "title": title });
    if let Some(b) = body { params["body"] = json!(b); }
    if let Some(s) = surface_id { params["surface_id"] = json!(s); }
    v2_call("notification.create", params)?;
    Ok(())
}

/// Create a new workspace. Returns workspace_id.
pub fn workspace_create(name: Option<&str>, cwd: Option<&str>) -> Result<String> {
    let mut params = json!({});
    if let Some(n) = name { params["name"] = json!(n); }
    if let Some(c) = cwd { params["cwd"] = json!(c); }
    let result = v2_call("workspace.create", params)?;
    result.get("workspace_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| eyre::eyre!("workspace.create did not return workspace_id"))
}

/// List all workspaces.
pub fn workspace_list() -> Result<Value> {
    v2_call("workspace.list", json!({}))
}

/// Get full system tree.
pub fn system_tree() -> Result<Value> {
    v2_call("system.tree", json!({}))
}
