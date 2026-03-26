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

/// Read the NATS hub URL from the `RZ_HUB` environment variable.
pub fn hub_url() -> Option<String> {
    std::env::var("RZ_HUB").ok().filter(|s| !s.is_empty())
}

/// Return `true` if `RZ_HUB` is set and a connection to the server succeeds.
pub fn check_hub() -> bool {
    let url = match hub_url() {
        Some(u) => u,
        None => return false,
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return false,
    };
    rt.block_on(async { async_nats::connect(&url).await.is_ok() })
}

/// Stream name for a given agent. Alphanumeric + dots only (NATS restriction).
fn stream_name(agent_name: &str) -> String {
    format!("RZ_{}", agent_name.replace('.', "_").replace('-', "_"))
}

/// Subject for a given agent.
fn subject(agent_name: &str) -> String {
    format!("agent.{agent_name}")
}

/// Durable consumer name for a given agent.
fn consumer_name(agent_name: &str) -> String {
    format!("rz_{agent_name}")
}

/// Ensure a JetStream stream exists for the target agent.
/// Creates it lazily on first send. Returns Ok(()) if JetStream is unavailable.
async fn ensure_stream(
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
    let url = hub_url().ok_or_else(|| eyre::eyre!("RZ_HUB not set"))?;
    let subj = subject(target_name);
    let json = serde_json::to_string(envelope)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;

        // Try JetStream first
        let js = async_nats::jetstream::new(client.clone());
        if ensure_stream(&js, target_name).await.is_ok() {
            match js
                .publish(
                    async_nats::Subject::from(subj.clone()),
                    json.clone().into_bytes().into(),
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
            .publish(async_nats::Subject::from(subj), json.into_bytes().into())
            .await
            .map_err(|e| eyre::eyre!("NATS publish failed: {e}"))?;
        client
            .flush()
            .await
            .map_err(|e| eyre::eyre!("NATS flush failed: {e}"))?;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}

/// Subscribe to messages for an agent and deliver them locally.
///
/// **Smart**: uses JetStream pull consumer if available (durable, replays
/// missed messages). Falls back to core NATS subscription.
///
/// Delivery mode is determined by the `delivery` string:
/// - `file:<name>` — write to the file-based mailbox via [`crate::mailbox::deliver`]
/// - `cmux:<surface_id>` — send via [`crate::cmux::send`]
/// - anything else — print the `@@RZ:` line to stdout
///
/// This function blocks forever (listener loop).
pub fn subscribe_and_deliver(agent_name: &str, delivery: &str) -> Result<()> {
    let url = hub_url().ok_or_else(|| eyre::eyre!("RZ_HUB not set"))?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;

        // Try JetStream consumer first
        let js = async_nats::jetstream::new(client.clone());
        if ensure_stream(&js, agent_name).await.is_ok() {
            match jetstream_consume(&js, agent_name, delivery).await {
                Ok(()) => return Ok(()), // won't return unless stream disappears
                Err(e) => {
                    eprintln!("rz: jetstream consume failed, falling back to core: {e}");
                }
            }
        }

        // Fallback: core NATS subscription
        core_subscribe(&client, agent_name, delivery).await
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
            // Don't ack — will be redelivered
            continue;
        }

        // Ack after successful delivery
        if let Err(e) = msg.ack().await {
            eprintln!("rz: jetstream ack failed: {e}");
        }

        // Pace cmux deliveries — the TUI needs time to process each message
        // before the next one arrives. Without this, rapid back-to-back
        // deliveries cause the second message to land in the input box
        // without Enter being pressed.
        if delivery.starts_with("cmux:") {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
    }

    bail!("JetStream subscription ended unexpectedly");
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
        }

        if delivery.starts_with("cmux:") {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
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
    } else {
        let wire = envelope.encode()?;
        println!("{wire}");
        Ok(())
    }
}
