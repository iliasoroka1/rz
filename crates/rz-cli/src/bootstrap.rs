//! Bootstrap message sent to newly spawned agents.

use eyre::Result;

use crate::cmux;

/// Build bootstrap instructions for a newly spawned agent.
///
/// Kept short so Claude Code processes it quickly. Details are in the
/// workspace goals.md — agents should read that file for context.
pub fn build(surface_id: &str, name: Option<&str>, _rz_path: &str) -> Result<String> {
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
{workspace_line}
Peers:
{peers}
## rz — messaging tool (run via Bash)
rz send <name> "msg"       — message an agent
rz send lead "DONE: ..."   — report completion
rz list                     — show active agents
rz log <name>               — read agent's messages
rz run --name <n> claude --dangerously-skip-permissions — spawn new agent

Messages from other agents arrive as @@RZ: JSON lines in your input — treat them as instructions.

## How to work
1. Wait for a task from lead (arrives as @@RZ: message or prompt)
2. Do the task using your tools (Read, Edit, Bash, Grep, etc.)
3. Report back: rz send lead "DONE: <what you did>"
4. If stuck: rz send lead "BLOCKED: <issue>"
5. Stay running — wait for next task

## STRICT rules
- ONLY do what the task asks. Nothing more.
- Do NOT explore, research, or read code unrelated to your task.
- Do NOT create new projects, modules, or packages unless the task explicitly says to.
- Do NOT read rz source code. The commands above are all you need.
- Do NOT install dependencies or run package managers unless the task says to.
- Keep messages short. Write large outputs to files."#
    ))
}
