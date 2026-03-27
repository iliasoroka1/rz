//! HTTP-to-NATS bridge — lets HTTP services participate as rz agents.
//!
//! Inbound:  subscribes to NATS `agent.<name>`, POSTs @@RZ: messages to webhook URL.
//! Outbound: exposes HTTP server on `--port`, agent POSTs to `/send` to publish to NATS.
//!
//! Usage: `rz bridge --name api-bot --webhook http://localhost:7070/inbox`

use eyre::Result;
use rz_agent_protocol::Envelope;
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Run the HTTP-to-NATS bridge. Blocks forever.
pub fn run_bridge(name: &str, webhook: &str, port: u16, permanent: bool) -> Result<()> {
    // Register in local + NATS registry
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let entry = crate::registry::AgentEntry {
        name: name.to_string(),
        id: format!("bridge-{}", std::process::id()),
        transport: "nats".to_string(),
        endpoint: name.to_string(),
        capabilities: vec![],
        permanent,
        registered_at: now_ms,
        last_seen: now_ms,
    };
    crate::registry::register(entry.clone())?;
    let _ = crate::registry::nats_register(&entry);

    eprintln!("rz: bridge '{name}' started");
    eprintln!("rz: inbound:  NATS agent.{name} → POST {webhook}");
    eprintln!("rz: outbound: http://0.0.0.0:{port}/send → NATS");

    // Channel for outbound messages (HTTP server → NATS publisher)
    let (out_tx, out_rx) = mpsc::channel::<(String, String)>(); // (to, text)

    // Heartbeat thread
    let hb_name = name.to_string();
    let hb_entry = entry.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(120));
        let _ = crate::registry::nats_heartbeat(&hb_name, &hb_entry);
    });

    // Outbound NATS publisher thread
    let pub_name = name.to_string();
    std::thread::spawn(move || {
        let name = pub_name;
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("rz: bridge: tokio build failed: {e}");
                return;
            }
        };
        rt.block_on(async {
            let url = match crate::nats_hub::hub_url() {
                Some(u) => u,
                None => {
                    eprintln!("rz: bridge: RZ_HUB not set — outbound disabled");
                    return;
                }
            };
            let client = match async_nats::connect(&url).await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("rz: bridge: NATS connect failed: {e}");
                    return;
                }
            };
            while let Ok((to, text)) = out_rx.recv() {
                let envelope = Envelope::new(
                    name.to_string(),
                    rz_agent_protocol::MessageKind::Chat { text },
                ).with_to(&to);
                if let Ok(wire) = envelope.encode() {
                    let subject = format!("agent.{to}");
                    let _ = client.publish(
                        async_nats::Subject::from(subject),
                        wire.into(),
                    ).await;
                    let _ = client.flush().await;
                }
            }
        });
    });

    // Outbound HTTP server thread
    let out_tx_http = out_tx.clone();
    let http_name = name.to_string();
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(format!("0.0.0.0:{port}")) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("rz: bridge: HTTP bind failed on port {port}: {e}");
                return;
            }
        };
        eprintln!("rz: bridge: outbound HTTP server on :{port}");

        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut reader = std::io::BufReader::new(&stream);
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }

            // Read headers
            let mut content_length: usize = 0;
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
                    break;
                }
                if let Some(val) = header.strip_prefix("Content-Length: ").or_else(|| header.strip_prefix("content-length: ")) {
                    content_length = val.trim().parse().unwrap_or(0);
                }
            }

            // Read body
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                if std::io::Read::read_exact(&mut reader, &mut body).is_err() {
                    continue;
                }
            }

            // Parse: POST /send with JSON body {"to": "name", "text": "message"}
            if request_line.starts_with("POST /send") {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&body) {
                    let to = json.get("to").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let text = json.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if !to.is_empty() && !text.is_empty() {
                        let _ = out_tx_http.send((to, text));
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{{\"ok\":true,\"from\":\"{http_name}\"}}"
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        continue;
                    }
                }
                let resp = "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"error\":\"need {\\\"to\\\":\\\"name\\\",\\\"text\\\":\\\"msg\\\"}\"}";
                let _ = stream.write_all(resp.as_bytes());
            } else if request_line.starts_with("GET /health") {
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{{\"name\":\"{http_name}\",\"status\":\"ok\"}}"
                );
                let _ = stream.write_all(resp.as_bytes());
            } else {
                let resp = "HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });

    // Inbound: NATS subscriber → POST to webhook
    let inbound_name = name.to_string();
    let webhook = webhook.to_string();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async {
        let url = crate::nats_hub::hub_url()
            .ok_or_else(|| eyre::eyre!("RZ_HUB not set — bridge requires NATS"))?;

        let client = async_nats::connect(&url)
            .await
            .map_err(|e| eyre::eyre!("NATS connect failed: {e}"))?;

        let subject = format!("agent.{inbound_name}");

        // Try JetStream first
        let js = async_nats::jetstream::new(client.clone());
        let stream_name = format!("RZ_{}", inbound_name.replace('.', "_").replace('-', "_"));

        if let Ok(stream) = js.get_stream(&stream_name).await {
            use async_nats::jetstream::consumer;
            use futures::StreamExt;

            let consumer_name = format!("rz_bridge_{inbound_name}");
            if let Ok(consumer) = stream
                .get_or_create_consumer(
                    &consumer_name,
                    consumer::pull::Config {
                        durable_name: Some(consumer_name.clone()),
                        ack_policy: consumer::AckPolicy::Explicit,
                        ..Default::default()
                    },
                )
                .await
            {
                if let Ok(mut messages) = consumer.messages().await {
                    eprintln!("rz: bridge: inbound listening (jetstream) for '{inbound_name}'");
                    while let Some(Ok(msg)) = messages.next().await {
                        let payload = std::str::from_utf8(&msg.payload).unwrap_or_default();
                        forward_to_webhook(&webhook, payload).await;
                        let _ = msg.ack().await;
                    }
                }
            }
        }

        // Fallback: core NATS
        let mut sub = client
            .subscribe(async_nats::Subject::from(subject))
            .await
            .map_err(|e| eyre::eyre!("NATS subscribe failed: {e}"))?;

        eprintln!("rz: bridge: inbound listening (core nats) for '{inbound_name}'");
        use futures::StreamExt;
        while let Some(msg) = sub.next().await {
            let payload = std::str::from_utf8(&msg.payload).unwrap_or_default();
            forward_to_webhook(&webhook, payload).await;
        }

        eyre::bail!("NATS subscription ended")
    })?;

    // Cleanup
    if !permanent {
        let _ = crate::registry::deregister(name);
        let _ = crate::registry::nats_deregister(name);
    }

    Ok(())
}

/// POST a message to the webhook URL.
async fn forward_to_webhook(webhook: &str, payload: &str) {
    // Parse the @@RZ: envelope to extract a clean JSON for the HTTP agent
    let stripped = payload.strip_prefix("@@RZ:").unwrap_or(payload);
    let body = if let Ok(env) = Envelope::decode(stripped) {
        // Send a simplified JSON that HTTP agents can easily consume
        serde_json::json!({
            "id": env.id,
            "from": env.from,
            "to": env.to,
            "text": match &env.kind {
                rz_agent_protocol::MessageKind::Chat { text } => text.clone(),
                _ => format!("{:?}", env.kind),
            },
            "ts": env.ts,
            "raw": payload,
        }).to_string()
    } else {
        serde_json::json!({ "raw": payload }).to_string()
    };

    let _ = std::process::Command::new("curl")
        .args(["-s", "-X", "POST", "-H", "Content-Type: application/json",
               "--max-time", "5", "-d", &body, webhook])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}
