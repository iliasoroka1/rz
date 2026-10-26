//! Keep @@RZ: lines short enough to paste into a terminal UI reliably.
//!
//! A multiplexer delivers a message by pasting it into the receiving pane.
//! Long pastes reach TUIs such as Claude Code in ~1 KB pieces, and under load
//! a piece can be dropped: a 2.8 KB report once arrived with 2 KB missing from
//! its middle while short messages always arrived intact. So an envelope over
//! [`PASTE_LIMIT`] is written to `~/.rz/messages/<id>.txt` (full text) and
//! `<id>.json` (full envelope), and the pane receives the same envelope with
//! the text cut to a preview plus the path to the full message.

use std::path::{Path, PathBuf};

use eyre::{Result, WrapErr};
use serde_json::Value;

/// Longest @@RZ: line pasted as is.
pub const PASTE_LIMIT: usize = 800;
/// Characters of the original text kept in the pasted preview.
const PREVIEW_CHARS: usize = 360;
const PREFIX: &str = "@@RZ:";

fn messages_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".rz").join("messages")
}

/// The line to paste for `wire`: unchanged when short (or not an envelope),
/// otherwise a preview envelope pointing at the spilled full message.
pub fn for_terminal(wire: &str) -> Result<String> {
    spill_into(&messages_dir(), wire)
}

fn spill_into(dir: &Path, wire: &str) -> Result<String> {
    if wire.len() <= PASTE_LIMIT {
        return Ok(wire.to_string());
    }
    let Some(json) = wire.strip_prefix(PREFIX) else {
        return Ok(wire.to_string());
    };
    let Ok(mut envelope) = serde_json::from_str::<Value>(json) else {
        return Ok(wire.to_string());
    };
    let id = envelope
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:x}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()));

    std::fs::create_dir_all(dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
    let json_path = dir.join(format!("{id}.json"));
    std::fs::write(&json_path, json).wrap_err_with(|| format!("writing {}", json_path.display()))?;

    let text = envelope
        .pointer("/kind/body/text")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let preview = match text {
        Some(text) => {
            let txt_path = dir.join(format!("{id}.txt"));
            std::fs::write(&txt_path, &text)
                .wrap_err_with(|| format!("writing {}", txt_path.display()))?;
            let head: String = text.chars().take(PREVIEW_CHARS).collect();
            format!(
                "{head}…\n[rz: long message, {} chars. Read the full text: {}]",
                text.chars().count(),
                txt_path.display()
            )
        }
        None => format!(
            "[rz: long {} envelope. Read it in full: {}]",
            envelope.pointer("/kind/kind").and_then(Value::as_str).unwrap_or("structured"),
            json_path.display()
        ),
    };
    match envelope.pointer_mut("/kind/body/text") {
        Some(slot) => *slot = Value::String(preview),
        None => {
            if let Some(kind) = envelope.get_mut("kind").and_then(Value::as_object_mut) {
                kind.insert("kind".into(), Value::String("chat".into()));
                kind.insert("body".into(), serde_json::json!({ "text": preview }));
            }
        }
    }
    Ok(format!("{PREFIX}{}", serde_json::to_string(&envelope)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(id: &str, text: &str) -> String {
        let env = serde_json::json!({
            "id": id, "from": "tagger", "to": "lead",
            "kind": {"kind": "chat", "body": {"text": text}}, "ts": 1
        });
        format!("{PREFIX}{env}")
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("rz-spill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn short_messages_pass_through() {
        let wire = chat("a1", "DONE: small");
        assert_eq!(spill_into(&tmp(), &wire).unwrap(), wire);
    }

    #[test]
    fn long_chat_is_spilled_with_a_preview() {
        let dir = tmp();
        let text = format!("REPORT `with` $vars\n{}", "x".repeat(3000));
        let out = spill_into(&dir, &chat("b2", &text)).unwrap();
        assert!(out.len() <= PASTE_LIMIT, "pasted line is {} bytes", out.len());

        let env: Value = serde_json::from_str(out.strip_prefix(PREFIX).unwrap()).unwrap();
        assert_eq!(env["id"], "b2");
        assert_eq!(env["from"], "tagger");
        let preview = env["kind"]["body"]["text"].as_str().unwrap();
        assert!(preview.starts_with("REPORT `with` $vars\n"));
        assert!(preview.contains(&dir.join("b2.txt").display().to_string()));

        assert_eq!(std::fs::read_to_string(dir.join("b2.txt")).unwrap(), text);
        let full: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("b2.json")).unwrap()).unwrap();
        assert_eq!(full["kind"]["body"]["text"], text);
    }

    #[test]
    fn non_envelopes_are_left_alone() {
        let raw = "y".repeat(2000);
        assert_eq!(spill_into(&tmp(), &raw).unwrap(), raw);
    }
}
