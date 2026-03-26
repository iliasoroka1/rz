//! Transport abstraction layer for routing messages.
//!
//! Supports three backends:
//! - **Cmux** — sends via cmux surface (the default, wraps `cmux::send`)
//! - **File** — writes envelope JSON to a file-based mailbox
//! - **Http** — POSTs envelope JSON to a remote endpoint (shells out to curl)

use eyre::{bail, Result};
use rz_protocol::Envelope;
use std::fs;
use std::path::PathBuf;

use crate::cmux;

/// Available transport backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Cmux,
    File,
    Http,
    #[cfg(feature = "nats")]
    Nats,
}

/// Parse a transport type string into a `Transport` variant.
pub fn parse_transport(s: &str) -> Result<Transport> {
    match s.to_lowercase().as_str() {
        "cmux" => Ok(Transport::Cmux),
        "file" => Ok(Transport::File),
        "http" | "https" => Ok(Transport::Http),
        #[cfg(feature = "nats")]
        "nats" => Ok(Transport::Nats),
        other => bail!("unknown transport type: '{}' (expected cmux, file, or http)", other),
    }
}

/// Deliver an envelope via the given transport.
///
/// The meaning of `endpoint` depends on the transport:
/// - `Cmux` — the target surface ID
/// - `File` — the agent name (mailbox written to `~/.rz/mailboxes/<name>/inbox/`)
/// - `Http` — the base URL (envelope POSTed to `<url>/rz/inbox`)
pub fn deliver(transport: &Transport, endpoint: &str, envelope: &Envelope) -> Result<()> {
    match transport {
        Transport::Cmux => deliver_cmux(endpoint, envelope),
        Transport::File => deliver_file(endpoint, envelope),
        Transport::Http => deliver_http(endpoint, envelope),
        #[cfg(feature = "nats")]
        Transport::Nats => deliver_nats(endpoint, envelope),
    }
}

/// High-level send: parse transport type, then deliver.
pub fn send_message(
    _name: &str,
    endpoint: &str,
    transport_type: &str,
    envelope: &Envelope,
) -> Result<()> {
    let transport = parse_transport(transport_type)?;
    deliver(&transport, endpoint, envelope)
}

// ── Cmux ────────────────────────────────────────────────────────────────

fn deliver_cmux(surface_id: &str, envelope: &Envelope) -> Result<()> {
    let wire = envelope.encode()?;
    cmux::send(surface_id, &wire)
}

// ── File ────────────────────────────────────────────────────────────────

fn mailbox_dir(name: &str) -> Result<PathBuf> {
    let home = std::env::var("HOME").map_err(|_| eyre::eyre!("HOME not set"))?;
    Ok(PathBuf::from(home).join(".rz").join("mailboxes").join(name).join("inbox"))
}

fn deliver_file(agent_name: &str, envelope: &Envelope) -> Result<()> {
    let dir = mailbox_dir(agent_name)?;
    fs::create_dir_all(&dir)?;

    let filename = format!("{}_{}.json", envelope.ts, envelope.id);
    let path = dir.join(filename);

    let json = serde_json::to_string_pretty(envelope)?;
    fs::write(&path, json)?;
    Ok(())
}

#[cfg(feature = "nats")]
fn deliver_nats(endpoint: &str, envelope: &Envelope) -> Result<()> {
    crate::nats_hub::publish(endpoint, envelope)
}

// ── Http ────────────────────────────────────────────────────────────────

fn deliver_http(base_url: &str, envelope: &Envelope) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_transport() {
        assert_eq!(parse_transport("cmux").unwrap(), Transport::Cmux);
        assert_eq!(parse_transport("CMUX").unwrap(), Transport::Cmux);
        assert_eq!(parse_transport("file").unwrap(), Transport::File);
        assert_eq!(parse_transport("http").unwrap(), Transport::Http);
        assert_eq!(parse_transport("https").unwrap(), Transport::Http);
        #[cfg(feature = "nats")]
        assert_eq!(parse_transport("nats").unwrap(), Transport::Nats);
        assert!(parse_transport("tcp").is_err());
    }
}
