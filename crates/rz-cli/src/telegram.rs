//! Telegram-to-NATS bridge — makes a Telegram chat appear as a regular rz agent.
//!
//! Long-polls Telegram for messages, publishes them to NATS. Subscribes to a
//! NATS subject, forwards agent replies to Telegram. Registers in KV for
//! discovery via `rz list --all`.

use std::collections::HashMap;
use std::time::Duration;

use eyre::{Result, bail};
use futures::StreamExt;

use rz_agent_protocol::{Envelope, MessageKind};

const LONG_POLL_TIMEOUT: u64 = 30;
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_MESSAGE_LEN: usize = 4096;
const MAX_REF_MAP_SIZE: usize = 100;
const KV_REFRESH_SECS: u64 = 20;

/// Telegram-to-NATS bridge.
pub struct TelegramBridge {
    token: String,
    chat_id: i64,
    name: String,
    default_agent: String,
}

impl TelegramBridge {
    pub fn new(
        token: String,
        chat_id: i64,
        name: String,
        default_agent: String,
    ) -> Self {
        Self { token, chat_id, name, default_agent }
    }

    fn api_url(&self) -> String {
        format!("https://api.telegram.org/bot{}", self.token)
    }

    fn own_subject(&self) -> String {
        format!("agent.{}", self.name)
    }

    fn from_identity(&self) -> String {
        self.own_subject()
    }

    /// Run the bridge. Long-polls Telegram + subscribes to NATS.
    /// Blocks until Ctrl-C or an unrecoverable error.
    pub async fn run(
        &self,
        nats_client: &async_nats::Client,
        kv: Option<&async_nats::jetstream::kv::Store>,
    ) -> Result<()> {
        let http = reqwest::Client::new();
        let api = self.api_url();

        // Validate the bot token.
        validate_token(&http, &api).await?;

        // Subscribe to our NATS subject for inbound agent messages.
        let subject = self.own_subject();
        let mut nats_sub = nats_client
            .subscribe(async_nats::Subject::from(subject.clone()))
            .await
            .map_err(|e| eyre::eyre!("NATS subscribe to {subject}: {e}"))?;
        eprintln!("subscribed to NATS subject: {subject}");

        // Register in KV immediately.
        self.register_kv(kv).await;

        let mut offset: Option<i64> = None;
        let mut backoff = INITIAL_BACKOFF;
        let mut ref_map = RefMap::new();
        let mut kv_interval = tokio::time::interval(Duration::from_secs(KV_REFRESH_SECS));

        loop {
            tokio::select! {
                // NATS → Telegram: agent messages forwarded to Telegram chat.
                msg = nats_sub.next() => {
                    let Some(msg) = msg else {
                        bail!("NATS subscription closed");
                    };
                    let payload = String::from_utf8_lossy(&msg.payload);
                    if let Ok(env) = Envelope::decode(&payload) {
                        if let Some(text) = extract_display_text(&env.kind) {
                            let sender = env.from.rsplit('.').next().unwrap_or(&env.from);
                            let display = format!("[{sender}] {text}");
                            if let Some(tg_msg_id) = send_message(&http, &api, self.chat_id, &display).await {
                                ref_map.insert(tg_msg_id, env.id.clone());
                            }
                        }
                    }
                }

                // Telegram → NATS: poll for new messages.
                updates = get_updates(&http, &api, &mut offset, &mut backoff) => {
                    for update in updates {
                        if let Some(parsed) = parse_update(&update) {
                            // Only process messages from the configured chat.
                            if parsed.chat_id != self.chat_id {
                                continue;
                            }

                            // Route: @agent prefix or default target.
                            let (target_agent, text) = parse_target(&parsed.text, &self.default_agent);
                            let target_subject = format!("agent.{target_agent}");

                            // Build envelope.
                            let mut envelope = Envelope::chat(
                                &self.from_identity(),
                                text,
                            );

                            // Correlate replies via ref map.
                            if let Some(reply_to_msg_id) = parsed.reply_to_message_id {
                                if let Some(rz_id) = ref_map.get(reply_to_msg_id) {
                                    envelope = envelope.with_ref(rz_id);
                                }
                            }

                            // Publish to NATS.
                            let wire = match envelope.encode() {
                                Ok(w) => w,
                                Err(e) => {
                                    eprintln!("encode error: {e}");
                                    continue;
                                }
                            };
                            // Publish via JetStream so durable consumers receive the message.
                            // Ensure stream exists, then publish.
                            let js = async_nats::jetstream::new(nats_client.clone());
                            let target_agent_name = target_agent.to_string();
                            let stream_name = format!("RZ_{}", target_agent_name.replace('.', "_").replace('-', "_"));
                            let _ = js.get_or_create_stream(async_nats::jetstream::stream::Config {
                                name: stream_name,
                                subjects: vec![target_subject.clone()],
                                max_messages: 10_000,
                                ..Default::default()
                            }).await;
                            match js.publish(
                                async_nats::Subject::from(target_subject.clone()),
                                wire.into_bytes().into(),
                            ).await {
                                Ok(ack_future) => {
                                    if let Err(e) = ack_future.await {
                                        eprintln!("NATS jetstream ack failed for {target_subject}: {e}");
                                    }
                                }
                                Err(e) => {
                                    eprintln!("NATS publish to {target_subject}: {e}");
                                    continue;
                                }
                            }
                            eprintln!("  → {target_subject}: {}", truncate(text, 80));
                        }
                    }
                }

                // Periodic KV refresh.
                _ = kv_interval.tick() => {
                    self.register_kv(kv).await;
                }

                // Ctrl-C.
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("stopping telegram bridge");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn register_kv(&self, kv: Option<&async_nats::jetstream::kv::Store>) {
        let Some(kv) = kv else { return };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let value = serde_json::json!({
            "name": self.name,
            "id": format!("telegram-{}", self.chat_id),
            "transport": "nats",
            "endpoint": self.name,
            "capabilities": [],
            "permanent": true,
            "registered_at": now_ms,
            "last_seen": now_ms,
        });
        let _ = kv.put(&self.name, value.to_string().into()).await;
    }
}

// ---------------------------------------------------------------------------
// Telegram API helpers
// ---------------------------------------------------------------------------

async fn validate_token(client: &reqwest::Client, api: &str) -> Result<()> {
    let resp: serde_json::Value = client
        .post(format!("{api}/getMe"))
        .send()
        .await
        .map_err(|e| eyre::eyre!("getMe request failed: {e}"))?
        .json()
        .await
        .map_err(|e| eyre::eyre!("getMe parse failed: {e}"))?;

    if resp["ok"].as_bool() != Some(true) {
        let desc = resp["description"].as_str().unwrap_or("unknown error");
        bail!("Telegram getMe failed: {desc}");
    }

    let bot_name = resp["result"]["username"].as_str().unwrap_or("unknown");
    eprintln!("Telegram bot @{bot_name} connected");
    Ok(())
}

/// Long-poll Telegram for updates. Returns parsed update objects.
/// Manages backoff internally — on error, sleeps and returns empty.
async fn get_updates(
    client: &reqwest::Client,
    api: &str,
    offset: &mut Option<i64>,
    backoff: &mut Duration,
) -> Vec<serde_json::Value> {
    let mut params = serde_json::json!({
        "timeout": LONG_POLL_TIMEOUT,
        "allowed_updates": ["message"],
    });
    if let Some(off) = offset {
        params["offset"] = serde_json::json!(*off);
    }

    let result = client
        .post(format!("{api}/getUpdates"))
        .json(&params)
        .timeout(Duration::from_secs(LONG_POLL_TIMEOUT + 10))
        .send()
        .await;

    let resp = match result {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Telegram poll error: {e}, retrying in {backoff:?}");
            tokio::time::sleep(*backoff).await;
            *backoff = (*backoff * 2).min(MAX_BACKOFF);
            return Vec::new();
        }
    };

    let status = resp.status();

    if status.as_u16() == 429 {
        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let retry = body["parameters"]["retry_after"].as_u64().unwrap_or(5);
        eprintln!("Telegram rate limited, retry after {retry}s");
        tokio::time::sleep(Duration::from_secs(retry)).await;
        return Vec::new();
    }

    if !status.is_success() {
        eprintln!("Telegram getUpdates failed ({status}), retrying in {backoff:?}");
        tokio::time::sleep(*backoff).await;
        *backoff = (*backoff * 2).min(MAX_BACKOFF);
        return Vec::new();
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Telegram parse error: {e}");
            tokio::time::sleep(*backoff).await;
            *backoff = (*backoff * 2).min(MAX_BACKOFF);
            return Vec::new();
        }
    };

    *backoff = INITIAL_BACKOFF;

    let Some(updates) = body["result"].as_array() else {
        return Vec::new();
    };

    // Advance offset past all received updates.
    for update in updates {
        if let Some(update_id) = update["update_id"].as_i64() {
            *offset = Some(update_id + 1);
        }
    }

    updates.clone()
}

/// Send a text message to Telegram. Returns the message_id on success.
/// Splits messages that exceed Telegram's 4096-char limit.
async fn send_message(
    client: &reqwest::Client,
    api: &str,
    chat_id: i64,
    text: &str,
) -> Option<i64> {
    let chunks = split_message(text);
    let mut last_msg_id = None;

    for chunk in &chunks {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "text": chunk,
        });

        let resp = match client
            .post(format!("{api}/sendMessage"))
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Telegram sendMessage error: {e}");
                return last_msg_id;
            }
        };

        if resp.status().is_success() {
            if let Ok(val) = resp.json::<serde_json::Value>().await {
                if let Some(msg_id) = val["result"]["message_id"].as_i64() {
                    last_msg_id = Some(msg_id);
                }
            }
        } else {
            let err = resp.text().await.unwrap_or_default();
            eprintln!("Telegram sendMessage failed: {err}");
        }
    }

    last_msg_id
}

/// Split text into chunks that fit Telegram's 4096-char limit.
fn split_message(text: &str) -> Vec<String> {
    if text.len() <= MAX_MESSAGE_LEN {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= MAX_MESSAGE_LEN {
            chunks.push(remaining.to_string());
            break;
        }

        let byte_limit = floor_char_boundary(remaining, MAX_MESSAGE_LEN);
        let search = &remaining[..byte_limit];
        let break_pos = search
            .rfind('\n')
            .or_else(|| search.rfind(' '))
            .map(|p| p + 1)
            .unwrap_or(byte_limit);

        chunks.push(remaining[..break_pos].to_string());
        remaining = remaining[break_pos..].trim_start();
    }

    chunks
}

fn floor_char_boundary(s: &str, max_bytes: usize) -> usize {
    if max_bytes >= s.len() {
        return s.len();
    }
    let mut i = max_bytes;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Message parsing
// ---------------------------------------------------------------------------

struct ParsedMessage {
    chat_id: i64,
    text: String,
    reply_to_message_id: Option<i64>,
}

fn parse_update(update: &serde_json::Value) -> Option<ParsedMessage> {
    let message = update.get("message")?;
    let text = message["text"].as_str()?;
    let chat_id = message["chat"]["id"].as_i64()?;
    let reply_to_message_id = message
        .get("reply_to_message")
        .and_then(|r| r["message_id"].as_i64());

    Some(ParsedMessage {
        chat_id,
        text: text.to_string(),
        reply_to_message_id,
    })
}

/// Parse `@agent_name rest of message` routing prefix.
/// Returns (target_agent, message_text).
fn parse_target<'a>(text: &'a str, default_agent: &'a str) -> (&'a str, &'a str) {
    if let Some(rest) = text.strip_prefix('@') {
        if let Some(space_pos) = rest.find(' ') {
            let agent = &rest[..space_pos];
            let msg = rest[space_pos..].trim_start();
            if !agent.is_empty() && !msg.is_empty() {
                return (agent, msg);
            }
        }
    }
    (default_agent, text)
}

/// Extract displayable text from an envelope kind.
/// Returns None for protocol-internal messages (Ping, Pong, Hello).
fn extract_display_text(kind: &MessageKind) -> Option<String> {
    match kind {
        MessageKind::Chat { text } => Some(text.clone()),
        MessageKind::Error { message } => Some(format!("Error: {message}")),
        MessageKind::Timer { label } => Some(format!("Timer: {label}")),
        MessageKind::Status { state, detail } => {
            Some(format!("[{state}] {detail}"))
        }
        MessageKind::ToolCall { name, .. } => {
            Some(format!("(calling tool: {name})"))
        }
        MessageKind::ToolResult { result, is_error, .. } => {
            let prefix = if *is_error { "Tool error" } else { "Tool result" };
            Some(format!("{prefix}: {}", truncate(result, 200)))
        }
        MessageKind::Delegate { task, .. } => {
            Some(format!("(delegating: {})", truncate(task, 200)))
        }
        // Internal protocol — don't forward.
        MessageKind::Ping
        | MessageKind::Pong
        | MessageKind::Hello { .. } => None,
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let end = floor_char_boundary(s, max);
        &s[..end]
    }
}

// ---------------------------------------------------------------------------
// Bounded ref map: telegram_message_id → rz_envelope_id
// ---------------------------------------------------------------------------

struct RefMap {
    map: HashMap<i64, String>,
    order: Vec<i64>,
}

impl RefMap {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            order: Vec::new(),
        }
    }

    fn insert(&mut self, tg_msg_id: i64, rz_id: String) {
        if self.order.len() >= MAX_REF_MAP_SIZE {
            if let Some(oldest) = self.order.first().copied() {
                self.map.remove(&oldest);
                self.order.remove(0);
            }
        }
        self.map.insert(tg_msg_id, rz_id);
        self.order.push(tg_msg_id);
    }

    fn get(&self, tg_msg_id: i64) -> Option<String> {
        self.map.get(&tg_msg_id).cloned()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_target_with_prefix() {
        let (agent, msg) = parse_target("@coder fix the tests", "default");
        assert_eq!(agent, "coder");
        assert_eq!(msg, "fix the tests");
    }

    #[test]
    fn parse_target_without_prefix() {
        let (agent, msg) = parse_target("just a message", "default");
        assert_eq!(agent, "default");
        assert_eq!(msg, "just a message");
    }

    #[test]
    fn parse_target_bare_at() {
        let (agent, msg) = parse_target("@", "default");
        assert_eq!(agent, "default");
        assert_eq!(msg, "@");
    }

    #[test]
    fn parse_target_at_no_space() {
        let (agent, msg) = parse_target("@agent", "default");
        assert_eq!(agent, "default");
        assert_eq!(msg, "@agent");
    }

    #[test]
    fn ref_map_bounded() {
        let mut rm = RefMap::new();
        for i in 0..150 {
            rm.insert(i, format!("id-{i}"));
        }
        assert!(rm.map.len() <= MAX_REF_MAP_SIZE);
        // Oldest entries evicted.
        assert!(rm.get(0).is_none());
        assert!(rm.get(149).is_some());
    }

    #[test]
    fn ref_map_get_returns_stored() {
        let mut rm = RefMap::new();
        rm.insert(42, "abc123".into());
        assert_eq!(rm.get(42), Some("abc123".into()));
        assert_eq!(rm.get(99), None);
    }

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0], "hello");
    }

    #[test]
    fn split_message_long() {
        let text = "a ".repeat(3000); // 6000 chars
        let chunks = split_message(&text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= MAX_MESSAGE_LEN);
        }
    }

    #[test]
    fn extract_display_text_chat() {
        let kind = MessageKind::Chat { text: "hello".into() };
        assert_eq!(extract_display_text(&kind), Some("hello".into()));
    }

    #[test]
    fn extract_display_text_ping_is_none() {
        assert_eq!(extract_display_text(&MessageKind::Ping), None);
    }

    #[test]
    fn extract_display_text_error() {
        let kind = MessageKind::Error { message: "boom".into() };
        assert_eq!(extract_display_text(&kind), Some("Error: boom".into()));
    }
}
