//! Pipe handler dispatch — routes CLI commands to registry and panes.

use rz_agent_protocol::{Envelope, MessageKind};
use serde::Serialize;
use zellij_tile::prelude::*;

use crate::registry::AgentRegistry;

// ---------------------------------------------------------------------------
// Response type
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct PipeResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Pipe dispatch
// ---------------------------------------------------------------------------

/// Handle an inbound pipe message with name "rz".
/// Caller has already verified `pipe_message.name == "rz"`.
pub fn handle_pipe(
    registry: &mut AgentRegistry,
    msg: &PipeMessage,
    timers: &mut Vec<crate::PendingTimer>,
    next_timer_id: &mut u64,
) {
    let action = match msg.args.get("action") {
        Some(a) => a.as_str(),
        None => {
            respond_error(msg, "missing 'action' arg");
            return;
        }
    };

    match action {
        "send" => handle_send(registry, msg),
        "broadcast" => handle_broadcast(registry, msg),
        "register" => handle_register(registry, msg),
        "unregister" => handle_unregister(registry, msg),
        "list" => handle_list(registry, msg),
        "status" => handle_status(registry, msg),
        "ping" => handle_ping(registry, msg),
        "timer" => handle_timer(registry, msg, timers, next_timer_id),
        "cancel_timer" => handle_cancel_timer(msg, timers),
        unknown => respond_error(msg, &format!("unknown action: {unknown}")),
    }
}

// ---------------------------------------------------------------------------
// Action handlers
// ---------------------------------------------------------------------------

fn handle_send(registry: &mut AgentRegistry, msg: &PipeMessage) {
    let target_raw = match msg.args.get("target") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: target");
            return;
        }
    };
    let from = match msg.args.get("from") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: from");
            return;
        }
    };
    let text = match &msg.payload {
        Some(p) => p.clone(),
        None => {
            respond_error(msg, "send requires a payload");
            return;
        }
    };

    // Resolve target: try pane ID, then name, then numeric shorthand.
    let target_pane = match resolve_target(registry, target_raw) {
        Some(id) => id,
        None => {
            respond_error(msg, &format!("unknown target: {target_raw}"));
            return;
        }
    };

    let envelope = Envelope::new(from, MessageKind::Chat { text })
        .maybe_with_ref(msg.args.get("ref").cloned());

    let msg_id = envelope.id.clone();
    deliver(&envelope, target_pane);

    // Touch both sender and receiver.
    if let Some(from_id) = parse_terminal_id(from) {
        registry.touch_message(from_id);
    }
    if let PaneId::Terminal(tid) = target_pane {
        registry.touch_message(tid);
    }

    respond_ok(msg, Some(serde_json::json!({ "message_id": msg_id })));
}

fn handle_broadcast(registry: &mut AgentRegistry, msg: &PipeMessage) {
    let from = match msg.args.get("from") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: from");
            return;
        }
    };
    let text = match &msg.payload {
        Some(p) => p.clone(),
        None => {
            respond_error(msg, "broadcast requires a payload");
            return;
        }
    };

    let envelope = Envelope::new(from, MessageKind::Chat { text })
        .maybe_with_ref(msg.args.get("ref").cloned());

    let from_id = parse_terminal_id(from);

    // Collect targets first to avoid borrow conflict.
    let targets: Vec<u32> = registry
        .get_active()
        .iter()
        .map(|e| e.pane_id)
        .filter(|id| from_id != Some(*id))
        .collect();

    for &tid in &targets {
        deliver(&envelope, PaneId::Terminal(tid));
        registry.touch_message(tid);
    }

    if let Some(fid) = from_id {
        registry.touch_message(fid);
    }

    let delivered = targets.len() as u32;

    respond_ok(msg, Some(serde_json::json!({ "delivered": delivered })));
}

fn handle_register(registry: &mut AgentRegistry, msg: &PipeMessage) {
    let pane_id_str = match msg.args.get("pane_id") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: pane_id");
            return;
        }
    };
    let pane_id = match parse_terminal_id(pane_id_str) {
        Some(id) => id,
        None => {
            respond_error(msg, &format!("invalid pane_id: {pane_id_str}"));
            return;
        }
    };
    let name = msg
        .args
        .get("name")
        .cloned()
        .unwrap_or_else(|| format!("terminal_{pane_id}"));
    let capabilities: Vec<String> = msg
        .args
        .get("capabilities")
        .map(|s| s.split(',').map(|c| c.trim().to_string()).collect())
        .unwrap_or_default();

    match registry.register(pane_id, name, capabilities) {
        Ok(_) => respond_ok(msg, None),
        Err(e) => respond_error(msg, &e.to_string()),
    }
}

fn handle_unregister(registry: &mut AgentRegistry, msg: &PipeMessage) {
    let pane_id_str = match msg.args.get("pane_id") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: pane_id");
            return;
        }
    };
    let pane_id = match parse_terminal_id(pane_id_str) {
        Some(id) => id,
        None => {
            respond_error(msg, &format!("invalid pane_id: {pane_id_str}"));
            return;
        }
    };

    match registry.unregister(pane_id) {
        Ok(()) => respond_ok(msg, None),
        Err(e) => respond_error(msg, &e.to_string()),
    }
}

fn handle_list(registry: &AgentRegistry, msg: &PipeMessage) {
    let agents: Vec<serde_json::Value> = registry
        .get_all()
        .iter()
        .map(|e| {
            serde_json::json!({
                "pane_id": format!("terminal_{}", e.pane_id),
                "name": e.name,
                "status": e.status,
                "command": e.command,
                "title": e.title,
                "tab": e.tab,
                "registered": e.registered,
                "capabilities": e.capabilities,
            })
        })
        .collect();

    respond_ok(msg, Some(serde_json::json!(agents)));
}

fn handle_status(registry: &AgentRegistry, msg: &PipeMessage) {
    let all = registry.get_all();
    let active = all
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                crate::registry::AgentStatus::Active | crate::registry::AgentStatus::Idle
            )
        })
        .count();
    let dead = all.len() - active;

    let data = serde_json::json!({
        "total": all.len(),
        "active": active,
        "dead": dead,
        "tick": registry.tick(),
    });
    respond_ok(msg, Some(data));
}

fn handle_ping(registry: &mut AgentRegistry, msg: &PipeMessage) {
    let from = msg.args.get("from").map(|s| s.as_str()).unwrap_or("hub");

    if let Some(target_raw) = msg.args.get("target") {
        // Forward ping to target pane.
        let target_pane = match resolve_target(registry, target_raw) {
            Some(id) => id,
            None => {
                respond_error(msg, &format!("unknown ping target: {target_raw}"));
                return;
            }
        };
        let envelope = Envelope::new(from, MessageKind::Ping);
        deliver(&envelope, target_pane);
        respond_ok(
            msg,
            Some(serde_json::json!({ "forwarded_to": format_pane_id(target_pane) })),
        );
    } else {
        // No target — hub replies with pong directly (health check).
        if let Some(from_id) = parse_terminal_id(from) {
            let envelope = Envelope::new("hub", MessageKind::Pong);
            deliver(&envelope, PaneId::Terminal(from_id));
        }
        respond_ok(msg, None);
    }
}

fn handle_timer(
    registry: &mut AgentRegistry,
    msg: &PipeMessage,
    timers: &mut Vec<crate::PendingTimer>,
    next_timer_id: &mut u64,
) {
    let target_raw = match msg.args.get("target") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: target");
            return;
        }
    };
    let seconds_str = match msg.args.get("seconds") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: seconds");
            return;
        }
    };
    let seconds: f64 = match seconds_str.parse() {
        Ok(v) if v > 0.0 => v,
        _ => {
            respond_error(msg, &format!("invalid seconds: {seconds_str}"));
            return;
        }
    };
    let label = msg.payload.clone().unwrap_or_default();

    let target_pane = match resolve_target(registry, target_raw) {
        Some(id) => id,
        None => {
            respond_error(msg, &format!("unknown target: {target_raw}"));
            return;
        }
    };

    let pane_id = match target_pane {
        PaneId::Terminal(id) => id,
        PaneId::Plugin(_) => {
            respond_error(msg, "timer target must be a terminal pane");
            return;
        }
    };

    let id = *next_timer_id;
    *next_timer_id += 1;

    timers.push(crate::PendingTimer {
        id,
        pane_id,
        label,
        seconds,
    });

    set_timeout(seconds);

    respond_ok(msg, Some(serde_json::json!({ "timer_id": id })));
}

fn handle_cancel_timer(
    msg: &PipeMessage,
    timers: &mut Vec<crate::PendingTimer>,
) {
    let id_str = match msg.args.get("timer_id") {
        Some(v) => v.as_str(),
        None => {
            respond_error(msg, "missing required arg: timer_id");
            return;
        }
    };
    let id: u64 = match id_str.parse() {
        Ok(v) => v,
        Err(_) => {
            respond_error(msg, &format!("invalid timer_id: {id_str}"));
            return;
        }
    };

    let before = timers.len();
    timers.retain(|t| t.id != id);
    if timers.len() < before {
        respond_ok(msg, Some(serde_json::json!({ "cancelled": id })));
    } else {
        respond_error(msg, &format!("timer {id} not found"));
    }
}

/// Deliver a Timer message to a terminal pane (called from update() on Event::Timer).
pub fn deliver_timer(pane_id: u32, label: &str) {
    let envelope = Envelope::new("hub", MessageKind::Timer { label: label.to_string() });
    deliver(&envelope, PaneId::Terminal(pane_id));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Resolve a target string to a PaneId.
/// Accepts: "terminal_3", "3" (numeric shorthand), or a registered name.
fn resolve_target(registry: &AgentRegistry, raw: &str) -> Option<PaneId> {
    // 1. Try as pane ID string ("terminal_3").
    if let Some(id) = parse_terminal_id(raw) {
        return Some(PaneId::Terminal(id));
    }

    // 2. Name lookup.
    if let Some(entry) = registry.lookup_by_name(raw) {
        return Some(PaneId::Terminal(entry.pane_id));
    }

    None
}

/// Parse "terminal_3" → Some(3), "3" → Some(3), "plugin_1" → None.
fn parse_terminal_id(s: &str) -> Option<u32> {
    if let Some(n) = s.strip_prefix("terminal_") {
        n.parse().ok()
    } else if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() {
        s.parse().ok()
    } else {
        None
    }
}

fn format_pane_id(pane_id: PaneId) -> String {
    match pane_id {
        PaneId::Terminal(n) => format!("terminal_{n}"),
        PaneId::Plugin(n) => format!("plugin_{n}"),
    }
}

/// Encode an envelope and write it to the target pane.
fn deliver(envelope: &Envelope, target: PaneId) {
    // encode() can fail on serialization error, but our envelopes are simple
    // structs that always serialize successfully.
    if let Ok(wire) = envelope.encode() {
        let bytes = format!("{wire}\r").into_bytes();
        write_to_pane_id(bytes, target);
    }
}

/// Send a success response to the CLI pipe caller.
fn respond_ok(msg: &PipeMessage, data: Option<serde_json::Value>) {
    let pipe_id = match &msg.source {
        PipeSource::Cli(id) => id,
        _ => return,
    };
    let resp = PipeResponse {
        ok: true,
        data,
        error: None,
    };
    if let Ok(json) = serde_json::to_string(&resp) {
        cli_pipe_output(pipe_id, &json);
        unblock_cli_pipe_input(pipe_id);
    }
}

/// Send an error response to the CLI pipe caller.
fn respond_error(msg: &PipeMessage, error: &str) {
    let pipe_id = match &msg.source {
        PipeSource::Cli(id) => id,
        _ => return,
    };
    let resp = PipeResponse {
        ok: false,
        data: None,
        error: Some(error.to_string()),
    };
    if let Ok(json) = serde_json::to_string(&resp) {
        cli_pipe_output(pipe_id, &json);
        unblock_cli_pipe_input(pipe_id);
    }
}
