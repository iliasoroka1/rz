//! rz — universal inter-agent communication.
//!
//! Supports multiple transports: cmux (terminal), file mailbox,
//! and HTTP. Agents register in a shared registry and messages
//! are routed via the appropriate transport.

pub mod bootstrap;
pub mod cmux;
pub mod log;
pub mod mailbox;
#[cfg(feature = "nats")]
pub mod nats_hub;
pub mod registry;
pub mod status;
pub mod transport;

pub use rz_protocol::{Envelope, MessageKind, SENTINEL};
