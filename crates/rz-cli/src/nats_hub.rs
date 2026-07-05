//! NATS transport module for cross-machine agent messaging.
//!
//! Connects to a NATS server specified by the `RZ_HUB` environment variable
//! (e.g. `nats://localhost:4222`) and routes messages over `agent.<name>` subjects.
//!
//! **Smart delivery**: uses JetStream when available for durable messaging —
//! messages survive agent restarts and offline periods. Falls back to core
//! NATS pub/sub if JetStream is not enabled on the server.

use eyre::{bail, Result};
use rz_agent_protocol::Envelope;
use std::sync::OnceLock;

/// Shared Tokio runtime for all NATS operations in this process.
/// Multi-thread flavor (1 worker) so `block_on` can be called concurrently
/// from different threads (e.g. a listener loop + its heartbeat thread).
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

/// Shared NATS client — one TCP connection per process instead of one per
/// operation. A single `rz send` used to open 2–3 connections (lookup,
/// KV, publish); now they all reuse this one.
pub fn client() -> Result<async_nats::Client> {
    static CLIENT: OnceLock<async_nats::Client> = OnceLock::new();
    if let Some(c) = CLIENT.get() {
        return Ok(c.clone());
    }
    let url = hub_url().ok_or_else(|| eyre::eyre!("RZ_HUB not set"))?;
    let c = runtime()
        .block_on(async_nats::connect(&url))
        .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;
    Ok(CLIENT.get_or_init(|| c).clone())
}

/// Read the NATS hub URL from the `RZ_HUB` environment variable.
/// Ignores values that don't look like URLs (e.g. `RZ_HUB=1`).
pub fn hub_url() -> Option<String> {
    std::env::var("RZ_HUB").ok().filter(|s| {
        if s.is_empty() {
            return false;
        }
        if !s.contains("://") {
            eprintln!("rz: ignoring RZ_HUB={s:?} — expected a nats:// URL");
            return false;
        }
        true
    })
}

/// Milliseconds to pause between terminal deliveries so the target TUI can
/// process each message before the next lands. Override with `RZ_PACE_MS`
/// (e.g. lower it for fast terminals, raise it for slow ones).
pub fn pace_ms() -> u64 {
    std::env::var("RZ_PACE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1500)
}

/// Sanitize a string for use in a JetStream stream name
/// (no dots, dashes, or spaces).
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Stream name for a given agent, scoped by session.
pub(crate) fn stream_name(agent_name: &str) -> String {
    format!(
        "RZ_{}_{}",
        sanitize(&crate::session::session_id()),
        sanitize(agent_name)
    )
}

/// Subject for a given agent: `agent.<session>.<name>`.
/// Session scoping means identically-named agents in different sessions
/// (or machines sharing one NATS server) never receive each other's traffic.
pub(crate) fn subject(agent_name: &str) -> String {
    format!("agent.{}.{agent_name}", crate::session::session_id())
}

/// Durable consumer name for a given agent (scoped per-stream, so the
/// session lives in the stream name).
pub(crate) fn consumer_name(agent_name: &str) -> String {
    format!("rz_{agent_name}")
}

/// Core-NATS request/reply subject a listener answers pings on.
/// A sibling of the stream subject so pings are synchronous liveness
/// probes, never stored or pasted to the agent.
fn ping_subject(agent_name: &str) -> String {
    format!("{}.ping", subject(agent_name))
}

/// Ping an agent's listener over core NATS request/reply.
/// Returns the round-trip time, or an error on timeout / no listener.
pub fn ping(target_name: &str, timeout: std::time::Duration) -> Result<std::time::Duration> {
    let client = client()?;
    let subj = ping_subject(target_name);

    runtime().block_on(async {
        let start = std::time::Instant::now();
        tokio::time::timeout(
            timeout,
            client.request(async_nats::Subject::from(subj), "ping".into()),
        )
        .await
        .map_err(|_| eyre::eyre!("no pong from '{target_name}' within {timeout:?} — is its listener running?"))?
        .map_err(|e| eyre::eyre!("ping failed: {e}"))?;
        Ok(start.elapsed())
    })
}

/// Ensure a JetStream stream exists for the target agent.
/// Creates it lazily on first use so both publishers and consumers get
/// durable delivery from the start. Errors only if JetStream is unavailable.
pub async fn ensure_stream(
    js: &async_nats::jetstream::Context,
    agent_name: &str,
) -> Result<()> {
    use async_nats::jetstream::stream;

    let name = stream_name(agent_name);
    let subj = subject(agent_name);

    // Try to get existing stream first
    match js.get_stream(&name).await {
        Ok(_) => return Ok(()),
        Err(_) => {} // doesn't exist, create it
    }

    js.create_stream(stream::Config {
        name,
        subjects: vec![subj],
        retention: stream::RetentionPolicy::WorkQueue, // consumed = deleted
        max_age: std::time::Duration::from_secs(7 * 24 * 3600), // 7 day TTL
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::Old,
        max_bytes: 50 * 1024 * 1024, // 50MB per agent
        ..Default::default()
    })
    .await
    .map_err(|e| eyre::eyre!("JetStream stream create failed: {e}"))?;

    Ok(())
}

/// Publish an envelope to the target agent.
///
/// **Smart**: tries JetStream first (durable). If JetStream is unavailable
/// on the server, falls back to core NATS pub/sub (fire-and-forget).
pub fn publish(target_name: &str, envelope: &Envelope) -> Result<()> {
    let json = serde_json::to_string(envelope)?;
    publish_bytes(target_name, json.into_bytes())
}

/// Publish raw bytes to the target agent's NATS subject.
///
/// Like [`publish`] but bypasses Envelope serialization — the caller
/// provides the exact bytes to put on the wire.
pub fn publish_raw(target_name: &str, raw: &[u8]) -> Result<()> {
    publish_bytes(target_name, raw.to_vec())
}

fn publish_bytes(target_name: &str, payload: Vec<u8>) -> Result<()> {
    let client = client()?;
    let subj = subject(target_name);

    runtime().block_on(async {
        // Try JetStream first
        let js = async_nats::jetstream::new(client.clone());
        if ensure_stream(&js, target_name).await.is_ok() {
            match js
                .publish(
                    async_nats::Subject::from(subj.clone()),
                    payload.clone().into(),
                )
                .await
            {
                Ok(ack_future) => {
                    // Wait for server ack — message is durably stored
                    match ack_future.await {
                        Ok(_) => return Ok(()),
                        Err(e) => {
                            eprintln!("rz: jetstream ack failed, falling back to core: {e}");
                        }
                    }
                }
                Err(e) => {
                    eprintln!("rz: jetstream publish failed, falling back to core: {e}");
                }
            }
        }

        // Fallback: core NATS pub/sub (fire-and-forget)
        client
            .publish(async_nats::Subject::from(subj), payload.into())
            .await
            .map_err(|e| eyre::eyre!("NATS publish failed: {e}"))?;
        client
            .flush()
            .await
            .map_err(|e| eyre::eyre!("NATS flush failed: {e}"))?;
        Ok::<(), eyre::Report>(())
    })
}

/// Wait on our own subject for an envelope whose `ref` matches `msg_id`.
/// Used by headless `rz ask` / `rz send --wait` — an ephemeral core
/// subscription that observes replies without consuming from the durable
/// stream (any running listener still receives and delivers them).
pub fn wait_for_ref(
    own_name: &str,
    msg_id: &str,
    timeout: std::time::Duration,
) -> Result<Envelope> {
    let client = client()?;
    let subj = subject(own_name);

    runtime().block_on(async {
        use futures::StreamExt;
        let mut sub = client
            .subscribe(async_nats::Subject::from(subj))
            .await
            .map_err(|e| eyre::eyre!("NATS subscribe failed: {e}"))?;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let msg = tokio::time::timeout_at(deadline, sub.next())
                .await
                .map_err(|_| {
                    eyre::eyre!("timeout ({}s) — no reply to {msg_id}", timeout.as_secs())
                })?
                .ok_or_else(|| eyre::eyre!("NATS subscription closed"))?;
            let payload = std::str::from_utf8(&msg.payload).unwrap_or_default();
            if let Ok(env) = Envelope::decode(payload) {
                if env.r#ref.as_deref() == Some(msg_id) {
                    return Ok(env);
                }
            }
        }
    })
}

/// Subscribe to messages for an agent and deliver them locally.
///
/// **Smart**: uses JetStream pull consumer if available (durable, replays
/// missed messages). Falls back to core NATS subscription.
///
/// Delivery mode is determined by the `delivery` string:
/// - `file:<name>` — write to the file-based mailbox via [`crate::mailbox::deliver`]
/// - `cmux:<surface_id>` — send via [`crate::cmux::send`]
/// - `zellij:<pane_id>` — send via [`crate::zellij::send`]
/// - `tmux:<pane_id>` — send via [`crate::tmux::send`]
/// - anything else — print the `@@RZ:` line to stdout
///
/// This function blocks forever (listener loop).
pub fn subscribe_and_deliver(agent_name: &str, delivery: &str) -> Result<()> {
    let client = client()?;

    runtime().block_on(async {
        let js = async_nats::jetstream::new(client.clone());

        // Answer liveness pings (core NATS request/reply) for as long as
        // this listener runs — `rz ping` works without involving the agent.
        {
            let ping_client = client.clone();
            let ping_subj = ping_subject(agent_name);
            tokio::spawn(async move {
                use futures::StreamExt;
                if let Ok(mut sub) = ping_client
                    .subscribe(async_nats::Subject::from(ping_subj))
                    .await
                {
                    while let Some(msg) = sub.next().await {
                        if let Some(reply) = msg.reply {
                            let _ = ping_client.publish(reply, "pong".into()).await;
                        }
                    }
                }
            });
        }

        // Retry loop — reconnect JetStream consumer if it drops
        loop {
            if ensure_stream(&js, agent_name).await.is_ok() {
                match jetstream_consume(&js, agent_name, delivery).await {
                    Ok(()) => {
                        eprintln!("rz: jetstream consumer ended for '{agent_name}', reconnecting in 2s...");
                    }
                    Err(e) if e.to_string().starts_with("FATAL_DELIVERY") => {
                        // Delivery is permanently broken — exit so the
                        // registry entry is removed and routing heals.
                        return Err(e);
                    }
                    Err(e) => {
                        eprintln!("rz: jetstream consume error for '{agent_name}': {e}, retrying in 2s...");
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }

            // Fallback: core NATS subscription (also retries on exit)
            match core_subscribe(&client, agent_name, delivery).await {
                Ok(()) => {
                    eprintln!("rz: core subscription ended for '{agent_name}', reconnecting in 2s...");
                }
                Err(e) if e.to_string().starts_with("FATAL_DELIVERY") => {
                    return Err(e);
                }
                Err(e) => {
                    eprintln!("rz: core subscribe error for '{agent_name}': {e}, retrying in 2s...");
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    })
}

/// JetStream pull consumer — durable, replays missed messages.
async fn jetstream_consume(
    js: &async_nats::jetstream::Context,
    agent_name: &str,
    delivery: &str,
) -> Result<()> {
    use async_nats::jetstream::consumer;
    use futures::StreamExt;

    let stream = js
        .get_stream(stream_name(agent_name))
        .await
        .map_err(|e| eyre::eyre!("get stream: {e}"))?;

    let consumer = stream
        .get_or_create_consumer(
            &consumer_name(agent_name),
            consumer::pull::Config {
                durable_name: Some(consumer_name(agent_name)),
                ack_policy: consumer::AckPolicy::Explicit,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| eyre::eyre!("create consumer: {e}"))?;

    eprintln!(
        "rz: listening (jetstream, durable) for agent '{agent_name}'"
    );

    let mut messages = consumer
        .messages()
        .await
        .map_err(|e| eyre::eyre!("consumer messages: {e}"))?;

    let mut consecutive_failures = 0u32;
    while let Some(msg) = messages.next().await {
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                eprintln!("rz: jetstream recv error: {e}");
                continue;
            }
        };

        let payload = std::str::from_utf8(&msg.payload).unwrap_or_default();
        let envelope = match Envelope::decode(payload) {
            Ok(env) => env,
            Err(e) => {
                eprintln!("rz: nats: bad envelope: {e}");
                // Ack bad messages so they don't redelivery forever
                let _ = msg.ack().await;
                continue;
            }
        };

        if let Err(e) = deliver_locally(delivery, &envelope) {
            eprintln!("rz: nats: delivery error: {e}");
            // A listener that can never deliver (e.g. detached from the cmux
            // socket) must die rather than sit on the durable consumer: its
            // exit deregisters it, so senders fall back to a working route
            // and un-acked messages redeliver to the next listener.
            consecutive_failures += 1;
            if consecutive_failures >= 5 {
                bail!("FATAL_DELIVERY: {} consecutive delivery failures, last: {e}", consecutive_failures);
            }
            // Don't ack — will be redelivered
            continue;
        }
        consecutive_failures = 0;

        // Ack after successful delivery
        if let Err(e) = msg.ack().await {
            eprintln!("rz: jetstream ack failed: {e}");
        }

        // Pace cmux deliveries — the TUI needs time to process each message
        // before the next one arrives. Without this, rapid back-to-back
        // deliveries cause the second message to land in the input box
        // without Enter being pressed.
        if delivery.starts_with("cmux:") || delivery.starts_with("zellij:") || delivery.starts_with("tmux:") {
            tokio::time::sleep(std::time::Duration::from_millis(pace_ms())).await;
        }
    }

    Ok(()) // Stream ended — caller will reconnect
}

/// Core NATS subscription — fire-and-forget, no persistence.
async fn core_subscribe(
    client: &async_nats::Client,
    agent_name: &str,
    delivery: &str,
) -> Result<()> {
    let subj = subject(agent_name);

    let mut subscriber = client
        .subscribe(async_nats::Subject::from(subj))
        .await
        .map_err(|e| eyre::eyre!("NATS subscribe failed: {e}"))?;

    eprintln!(
        "rz: listening (core nats, non-durable) for agent '{agent_name}'"
    );

    let mut consecutive_failures = 0u32;
    while let Some(msg) = futures::StreamExt::next(&mut subscriber).await {
        let payload = std::str::from_utf8(&msg.payload).unwrap_or_default();
        let envelope = match Envelope::decode(payload) {
            Ok(env) => env,
            Err(e) => {
                eprintln!("rz: nats: bad envelope: {e}");
                continue;
            }
        };

        if let Err(e) = deliver_locally(delivery, &envelope) {
            eprintln!("rz: nats: delivery error: {e}");
            consecutive_failures += 1;
            if consecutive_failures >= 5 {
                bail!("FATAL_DELIVERY: {consecutive_failures} consecutive delivery failures, last: {e}");
            }
        } else {
            consecutive_failures = 0;
        }

        if delivery.starts_with("cmux:") || delivery.starts_with("zellij:") || delivery.starts_with("tmux:") {
            tokio::time::sleep(std::time::Duration::from_millis(pace_ms())).await;
        }
    }

    bail!("NATS subscription ended unexpectedly");
}

/// Route an envelope to the appropriate local delivery mechanism.
fn deliver_locally(delivery: &str, envelope: &Envelope) -> Result<()> {
    if let Some(name) = delivery.strip_prefix("file:") {
        crate::mailbox::deliver(name, envelope)
    } else if let Some(surface_id) = delivery.strip_prefix("cmux:") {
        let wire = envelope.encode()?;
        crate::cmux::send(surface_id, &wire)
    } else if let Some(pane_id) = delivery.strip_prefix("zellij:") {
        let wire = envelope.encode()?;
        crate::zellij::send(pane_id, &wire)
    } else if let Some(pane_id) = delivery.strip_prefix("tmux:") {
        let wire = envelope.encode()?;
        crate::tmux::send(pane_id, &wire)
    } else {
        let wire = envelope.encode()?;
        println!("{wire}");
        Ok(())
    }
}
