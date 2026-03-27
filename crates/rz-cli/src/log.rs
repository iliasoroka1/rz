//! Helpers for extracting and formatting protocol messages from scrollback.

use rz_agent_protocol::{Envelope, MessageKind, SENTINEL};

/// Scan scrollback text for `@@RZ:` lines and parse each into an [`Envelope`].
///
/// Handles terminal line-wrapping by joining continuation lines until the
/// JSON parses successfully (up to 20 lines lookahead).
pub fn extract_messages(scrollback: &str) -> Vec<Envelope> {
    let lines: Vec<&str> = scrollback.lines().collect();
    let mut result = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if let Some(idx) = lines[i].find(SENTINEL) {
            let mut candidate = lines[i][idx..].to_string();
            if let Ok(env) = Envelope::decode(&candidate) {
                result.push(env);
                i += 1;
                continue;
            }
            // Try joining wrapped continuation lines.
            for j in 1..20 {
                if i + j >= lines.len() {
                    break;
                }
                candidate.push_str(lines[i + j]);
                if let Ok(env) = Envelope::decode(&candidate) {
                    result.push(env);
                    i += j;
                    break;
                }
            }
        }
        i += 1;
    }

    result
}

/// Format an envelope as a human-readable one-liner: `[HH:MM:SS] from_id> text`
///
/// If `own_id` is provided and matches `envelope.from`, appends `(me)` to the sender.
pub fn format_message(envelope: &Envelope, own_id: Option<&str>) -> String {
    let secs = envelope.ts / 1000;
    let h = (secs / 3600) % 24;
    let m = (secs % 3600) / 60;
    let s = secs % 60;

    let me = if own_id == Some(envelope.from.as_str()) { " (me)" } else { "" };

    let text = match &envelope.kind {
        MessageKind::Chat { text } => text.as_str(),
        MessageKind::Ping => "ping",
        MessageKind::Pong => "pong",
        MessageKind::Error { message } => {
            return format!("[{h:02}:{m:02}:{s:02}] {}{me}> error: {message}", envelope.from);
        }
        MessageKind::Timer { label } => {
            return format!("[{h:02}:{m:02}:{s:02}] {}{me}> timer: {label}", envelope.from);
        }
        MessageKind::Status { state, detail } => {
            return format!("[{h:02}:{m:02}:{s:02}] {}{me}> [{state}] {detail}", envelope.from);
        }
        MessageKind::ToolCall { name, .. } => {
            return format!("[{h:02}:{m:02}:{s:02}] {}{me}> (calling tool: {name})", envelope.from);
        }
        MessageKind::ToolResult { name, result, is_error } => {
            let prefix = if *is_error { "tool error" } else { "tool result" };
            let short = if result.len() > 200 { &result[..200] } else { result.as_str() };
            return format!("[{h:02}:{m:02}:{s:02}] {}{me}> {prefix} ({name}): {short}", envelope.from);
        }
        MessageKind::Delegate { task, .. } => {
            let short = if task.len() > 200 { &task[..200] } else { task.as_str() };
            return format!("[{h:02}:{m:02}:{s:02}] {}{me}> (delegating: {short})", envelope.from);
        }
        MessageKind::Hello { name } => {
            return format!("[{h:02}:{m:02}:{s:02}] {}{me}> hello from {name}", envelope.from);
        }
    };

    format!("[{h:02}:{m:02}:{s:02}] {}{me}> {text}", envelope.from)
}
