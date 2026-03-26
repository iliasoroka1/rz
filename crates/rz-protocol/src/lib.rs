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
}

static COUNTER: AtomicU32 = AtomicU32::new(0);

impl Envelope {
    pub fn new(from: impl Into<String>, kind: MessageKind) -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            id: format!("{:04x}{:04x}", (ts & 0xFFFF) as u16, seq),
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
