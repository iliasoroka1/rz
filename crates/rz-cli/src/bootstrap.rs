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
        r#"You are agent "{identity}" (surface: {surface_id}).

{workspace_line}Peers:
{peers}
## rz commands (use Bash tool to run these)
rz send <name> "msg"          — send message to agent by name
rz send lead "DONE: ..."      — report completion to lead
rz list                        — show active agents
rz log <name>                  — read agent's messages
rz run --name <n> claude --dangerously-skip-permissions  — spawn new Claude agent

Incoming messages appear as @@RZ: JSON lines in your input.

## Rules
- Work autonomously with your tools (Read, Edit, Bash, etc.)
- When done: rz send lead "DONE: <summary>"
- When blocked: rz send lead "BLOCKED: <issue>"
- Stay running after reporting — wait for next task
- Write large outputs to files, keep messages short
- Do NOT create Go modules, Python packages, or new projects unless explicitly asked
- Do NOT read rz source code — just use the commands above"#
    ))
}
