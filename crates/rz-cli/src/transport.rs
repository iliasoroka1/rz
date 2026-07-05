//! HTTP delivery for registered `http` transport agents.
//!
//! POSTs envelope JSON to `<base_url>/rz/inbox` (shells out to curl).
//! Other transports are handled directly: cmux via [`crate::cmux::send`],
//! file via [`crate::mailbox::deliver`], NATS via [`crate::nats_hub::publish`].

use eyre::{bail, Result};
use rz_agent_protocol::Envelope;

pub fn deliver_http(base_url: &str, envelope: &Envelope) -> Result<()> {
    let url = format!("{}/rz/inbox", base_url.trim_end_matches('/'));
    let json = serde_json::to_string(envelope)?;

    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "-f",
            "-X", "POST",
            "-H", "Content-Type: application/json",
            "-d", &json,
            &url,
        ])
        .output()
        .map_err(|e| eyre::eyre!("failed to run curl: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "HTTP POST to {} failed (exit {}): {}",
            url,
            output.status.code().unwrap_or(-1),
            stderr.trim()
        );
    }
    Ok(())
}
