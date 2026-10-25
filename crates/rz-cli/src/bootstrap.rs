//! Bootstrap message sent to newly spawned agents.

use eyre::Result;

/// Absolute path to the running rz binary. Spawned agents must use this
/// instead of whatever `rz` happens to be on their PATH — a version mismatch
/// there means they publish on subjects nobody is listening to.
fn rz_bin() -> String {
    std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "rz".into())
}

/// Build bootstrap instructions for a multiplexer agent.
///
/// `listener_running` — whether the spawner already started an `rz listen`
/// for this agent (rz run does). If so, the agent is told NOT to start
/// another one: two listeners share one durable consumer and split messages.
pub fn build(
    surface_id: &str,
    name: Option<&str>,
    backend: &dyn crate::backend::Backend,
    listener_running: bool,
) -> Result<String> {
    let identity = name.unwrap_or(surface_id);

    let mut peers = String::new();
    for p in backend.list_panes()? {
        if p.is_plugin || p.id == surface_id {
            continue;
        }
        let label = if p.title.is_empty() { &p.id } else { &p.title };
        peers.push_str(&format!("  - {label}\n"));
    }

    build_common(identity, &peers, Some(surface_id), listener_running)
}

/// Build bootstrap for a PTY agent (no multiplexer).
/// PTY agents receive messages through the wrapper's own NATS subscriber,
/// so they never need an external listener.
pub fn build_pty(name: &str) -> Result<String> {
    let mut peers = String::new();
    if let Ok(agents) = crate::registry::list_all() {
        for a in &agents {
            if a.name != name {
                peers.push_str(&format!("  - {}\n", a.name));
            }
        }
    }

    build_common(name, &peers, None, true)
}

fn build_common(
    identity: &str,
    peers: &str,
    surface_id: Option<&str>,
    listener_running: bool,
) -> Result<String> {
    let peer_list = if peers.is_empty() { "  (none)\n" } else { peers };
    let rz = rz_bin();

    let listener_instruction = if listener_running {
        "\n## Receiving messages\n\
         Your NATS listener is already running — messages from other agents \
         arrive automatically as @@RZ: JSON in your input. Do NOT start \
         another `rz listen`; a second one would compete for your messages."
            .to_string()
    } else if let Some(sid) = surface_id {
        format!(
            r#"
## IMPORTANT: Start your NATS listener
Run this IMMEDIATELY as your first action using a background bash command (run_in_background=true, timeout=600000):
```
{rz} listen {identity} --deliver cmux:{sid}
```
This lets you receive messages from other agents via NATS. Without it, you won't get any @@RZ: messages."#
        )
    } else {
        String::new()
    };

    Ok(format!(
        r#"You are agent "{identity}".

Peers:
{peer_list}
## rz commands (always use this exact binary path — plain `rz` on PATH may be a different version)
{rz} send <name> "msg"   — message an agent (short, plain text)
{rz} send lead "DONE: <summary>"  — report completion
For anything with markdown, `backticks`, $ or several lines, pipe it through a quoted heredoc —
inside "double quotes" the shell runs backticks and expands $vars, and parts of your message vanish:
{rz} send lead - <<'EOF'
DONE: <multi-line report>
EOF
{rz} run --name <n> claude --dangerously-skip-permissions  — spawn agent
{rz} ps                  — list agents
{rz} logs <name>         — read agent output
{listener_instruction}
## Rules
1. Do the task. Report: {rz} send lead "DONE: <what you did>"
2. If stuck: {rz} send lead "BLOCKED: <issue>"
3. Stay running after reporting — wait for next task.
4. Only do what is asked. Do not explore unrelated code.
5. Do not read rz source code — use the commands above."#
    ))
}
