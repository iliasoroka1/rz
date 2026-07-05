//! Wire protocol: JSON envelopes with `@@RZ:` sentinel.
//!
//! Every protocol message is a single line:
//! ```text
//! @@RZ:{"id":"...","from":"...","kind":{"kind":"chat","body":{"text":"..."}}}
//! ```
//! The `@@RZ:` prefix lets receivers distinguish protocol messages from
//! normal shell output or human typing.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};

pub const SENTINEL: &str = "@@RZ:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub id: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    pub kind: MessageKind,
    pub ts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum MessageKind {
    Chat { text: String },
    Ping,
    Pong,
    Error { message: String },
    Timer { label: String },
    Status { state: String, detail: String },
    ToolCall { name: String, input: String },
    ToolResult { name: String, result: String, is_error: bool },
    Delegate { task: String, to: Option<String> },
    Hello { name: String },
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// Build a message ID from timestamp, process id, and a per-process counter.
/// 16 hex chars: 32 bits of millis + 16 bits of pid + 16 bits of sequence,
/// so IDs stay unique across concurrent processes and restarts — replying
/// (`--ref`) and pong-matching depend on them not colliding.
pub fn message_id(ts_ms: u64, seq: u32) -> String {
    format!(
        "{:08x}{:04x}{:04x}",
        (ts_ms & 0xFFFF_FFFF) as u32,
        (std::process::id() & 0xFFFF) as u16,
        (seq & 0xFFFF) as u16
    )
}

impl Envelope {
    pub fn chat(from: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(from, MessageKind::Chat { text: text.into() })
    }

    pub fn new(from: impl Into<String>, kind: MessageKind) -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            id: message_id(ts, seq),
            to: None,
            r#ref: None,
            from: from.into(),
            kind,
            ts,
        }
    }

    /// Builder: set `to` for directed messaging.
    pub fn with_to(mut self, t: impl Into<String>) -> Self {
        self.to = Some(t.into());
        self
    }

    /// Builder: set `ref` for threading.
    pub fn with_ref(mut self, r: impl Into<String>) -> Self {
        self.r#ref = Some(r.into());
        self
    }

    /// Builder: conditionally set `ref`.
    pub fn maybe_with_ref(mut self, r: Option<String>) -> Self {
        self.r#ref = r;
        self
    }

    /// Encode to wire format: `@@RZ:<json>`
    pub fn encode(&self) -> eyre::Result<String> {
        let json = serde_json::to_string(self)?;
        Ok(format!("{SENTINEL}{json}"))
    }

    /// Decode from wire format (with or without sentinel prefix).
    pub fn decode(line: &str) -> eyre::Result<Self> {
        let payload = line.strip_prefix(SENTINEL).unwrap_or(line);
        Ok(serde_json::from_str(payload.trim())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let env = Envelope::chat("alice", "hello bob")
            .with_to("bob")
            .with_ref("cafe1234");
        let wire = env.encode().unwrap();
        assert!(wire.starts_with(SENTINEL));

        let back = Envelope::decode(&wire).unwrap();
        assert_eq!(back.id, env.id);
        assert_eq!(back.from, "alice");
        assert_eq!(back.to.as_deref(), Some("bob"));
        assert_eq!(back.r#ref.as_deref(), Some("cafe1234"));
        match back.kind {
            MessageKind::Chat { text } => assert_eq!(text, "hello bob"),
            other => panic!("wrong kind: {other:?}"),
        }
    }

    #[test]
    fn decode_without_sentinel() {
        let env = Envelope::chat("a", "b");
        let json = serde_json::to_string(&env).unwrap();
        let back = Envelope::decode(&json).unwrap();
        assert_eq!(back.id, env.id);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Envelope::decode("@@RZ:{not json").is_err());
        assert!(Envelope::decode("").is_err());
    }

    #[test]
    fn ids_are_unique_within_process() {
        let ids: std::collections::HashSet<String> =
            (0..1000).map(|_| Envelope::chat("a", "b").id).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn message_id_embeds_pid_and_seq() {
        let a = message_id(0x1122_3344_5566, 7);
        assert_eq!(a.len(), 16);
        assert!(a.starts_with("33445566"));
        assert!(a.ends_with("0007"));
        // Same inputs → same id; different seq → different id.
        assert_eq!(a, message_id(0x1122_3344_5566, 7));
        assert_ne!(a, message_id(0x1122_3344_5566, 8));
    }
}
