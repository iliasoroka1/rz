//! NATS transport module for cross-machine agent messaging.
//!
//! Connects to a NATS server specified by the `RZ_HUB` environment variable
//! (e.g. `nats://localhost:4222`) and routes messages over `agent.<name>` subjects.

use eyre::{bail, Result};
use rz_protocol::Envelope;

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
    rt.block_on(async {
        async_nats::connect(&url).await.is_ok()
    })
}

/// Publish an envelope to the NATS subject `agent.<target_name>`.
pub fn publish(target_name: &str, envelope: &Envelope) -> Result<()> {
    let url = hub_url().ok_or_else(|| eyre::eyre!("RZ_HUB not set"))?;
    let subject = format!("agent.{target_name}");
    let json = serde_json::to_string(envelope)?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url).await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;
        client.publish(async_nats::Subject::from(subject), json.into_bytes().into()).await
            .map_err(|e| eyre::eyre!("NATS publish failed: {e}"))?;
        client.flush().await
            .map_err(|e| eyre::eyre!("NATS flush failed: {e}"))?;
        Ok::<(), eyre::Report>(())
    })?;
    Ok(())
}

/// Subscribe to `agent.<agent_name>` and deliver incoming messages locally.
///
/// Delivery mode is determined by the `delivery` string:
/// - `file:<name>` — write to the file-based mailbox via [`crate::mailbox::deliver`]
/// - `cmux:<surface_id>` — send via [`crate::cmux::send`]
/// - anything else — print the `@@RZ:` line to stdout
///
/// This function blocks forever (listener loop).
pub fn subscribe_and_deliver(agent_name: &str, delivery: &str) -> Result<()> {
    let url = hub_url().ok_or_else(|| eyre::eyre!("RZ_HUB not set"))?;
    let subject = format!("agent.{agent_name}");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let client = async_nats::connect(&url).await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;

        let mut subscriber = client.subscribe(async_nats::Subject::from(subject)).await
            .map_err(|e| eyre::eyre!("NATS subscribe failed: {e}"))?;

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
        }

        bail!("NATS subscription ended unexpectedly");
    })
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
