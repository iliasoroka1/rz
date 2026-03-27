//! Bootstrap message sent to newly spawned agents.

use eyre::Result;

/// Build bootstrap instructions for a multiplexer agent.
pub fn build(surface_id: &str, name: Option<&str>, backend: &dyn crate::backend::Backend) -> Result<String> {
    let identity = name.unwrap_or(surface_id);

    let mut peers = String::new();
    for p in backend.list_panes()? {
        if p.is_plugin || p.id == surface_id {
            continue;
        }
        let label = if p.title.is_empty() { &p.id } else { &p.title };
        peers.push_str(&format!("  - {label}\n"));
    }

    build_common(identity, &peers)
}

/// Build bootstrap for a PTY agent (no multiplexer).
pub fn build_pty(name: &str) -> Result<String> {
    let mut peers = String::new();
    if let Ok(agents) = crate::registry::list_all() {
        for a in &agents {
            if a.name != name {
                peers.push_str(&format!("  - {}\n", a.name));
            }
        }
    }

    build_common(name, &peers)
}

fn build_common(identity: &str, peers: &str) -> Result<String> {
    let peer_list = if peers.is_empty() { "  (none)\n" } else { peers };

    Ok(format!(
        r#"You are agent "{identity}".

Peers:
{peer_list}
## rz commands
rz send <name> "msg"   — message an agent
rz send lead "DONE: <summary>"  — report completion
rz run --name <n> claude --dangerously-skip-permissions  — spawn agent
rz ps                  — list agents
rz logs <name>         — read agent output

Messages from other agents arrive as @@RZ: JSON in your input.

## Rules
1. Do the task. Report: rz send lead "DONE: <what you did>"
2. If stuck: rz send lead "BLOCKED: <issue>"
3. Stay running after reporting — wait for next task.
4. Only do what is asked. Do not explore unrelated code.
5. Do not read rz source code — use the commands above."#
    ))
}
