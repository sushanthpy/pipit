//! IDE Bridge Protocol
//!
//! A transport-agnostic bidirectional communication protocol between
//! the Pipit CLI agent and IDE extensions (VS Code, JetBrains, etc.).
//!
//! Architecture:
//! - Transport layer: SSE (server→client) + HTTP POST (client→server), WebSocket fallback
//! - Session management: creation, authentication, reconnection
//! - Messaging: typed message routing between IDE and agent
//! - UI integration: diff views, inline suggestions, progress updates

pub mod protocol;
pub mod session;
pub mod transport;

pub use protocol::{BridgeCommand, BridgeEvent, BridgeMessage, MessageId};
pub use session::{BridgeSession, SessionConfig};
pub use transport::{Transport, TransportConfig};
