//! rz — universal inter-agent communication.
//!
//! Supports multiple transports: cmux (terminal), file mailbox,
//! and HTTP. Agents register in a shared registry and messages
//! are routed via the appropriate transport.

pub mod backend;
pub mod bootstrap;
pub mod bridge;
pub mod pty;
pub mod cmux;
pub mod log;
pub mod mailbox;

pub mod nats_hub;
pub mod registry;
pub mod status;
pub mod transport;
pub mod tmux;
pub mod zellij;

pub use rz_agent_protocol::{Envelope, MessageKind, SENTINEL};
