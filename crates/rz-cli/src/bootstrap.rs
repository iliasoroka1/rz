//! Bootstrap message sent to newly spawned agents.

use eyre::Result;

use crate::cmux;

/// Build bootstrap instructions for a newly spawned agent.
///
/// Kept short so Claude Code processes it quickly. Details are in the
/// workspace goals.md — agents should read that file for context.
pub fn build(surface_id: &str, name: Option<&str>, rz_path: &str) -> Result<String> {
    let surfaces = cmux::list_surfaces()?;
    let identity = name.unwrap_or(surface_id);

    let mut peers = String::new();
    for s in &surfaces {
        if s.surface_type == "browser" || s.id == surface_id {
            continue;
        }
        let label = if s.title.is_empty() { "shell" } else { &s.title };
        peers.push_str(&format!("  - {} ({})\n", s.id, label));
    }
    if peers.is_empty() {
        peers.push_str("  (none)\n");
    }

    // Check if workspace exists.
    let workspace = std::env::var("CMUX_SOCKET_PATH")
        .ok()
        .and_then(|sock| {
            let stem = std::path::Path::new(&sock)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("cmux")
                .to_string();
            Some(format!("/tmp/rz-{stem}"))
        })
        .filter(|p| std::path::Path::new(p).exists());

    let workspace_line = if let Some(ref ws) = workspace {
        format!("Workspace: `{ws}/` — read `goals.md` on start, write large outputs to `shared/`.\n")
    } else {
        String::new()
    };

    Ok(format!(
        r#"You are agent "{identity}" (surface: {surface_id}) in a multi-agent session.

{workspace_line}Peers:
{peers}
Communication — use `{rz_path}` (aliased as `rz`):
  rz send <name|id> "msg"    — send message
  rz send lead "DONE: ..."   — report completion to lead
  rz ask <name|id> "question" — send and wait for reply
  rz broadcast "msg"          — send to all agents
  rz list                     — show active agents
  rz log <name|id>            — read agent's messages

Messages from other agents arrive as @@RZ: lines pasted into your input.

RULES:
1. Do your task using tools (Read, Edit, Bash, etc.) — work autonomously
2. When DONE, report back via: rz send lead "DONE: <what you did>"
3. If BLOCKED, report via: rz send lead "BLOCKED: <issue>"
4. Do NOT exit — wait for next task after reporting
5. Keep messages short — write large outputs to files"#
    ))
}
