//! pipit-mesh: Distributed multi-agent coordination via P2P mesh networking.
//!
//! Implements:
//! - SWIM gossip protocol for node discovery and failure detection
//! - mDNS/DNS-SD for zero-config LAN discovery
//! - CRDT shared state for multi-node consistency
//! - Task delegation engine with capability-based routing
//!
//! ## Architecture
//! ```text
//! ┌─ MeshDaemon ─────────────────────────┐
//! │  SwimProtocol  ←  gossip messages     │
//! │  NodeRegistry  ←  capability index    │
//! │  CrdtStore     ←  shared state        │
//! │  DelegationEngine ← task routing      │
//! └──────────────────────────────────────┘
//! ```

pub mod swim;
pub mod crdt;
pub mod node;
pub mod delegation;

pub use node::{NodeDescriptor, NodeId, MeshDaemon};
pub use swim::SwimProtocol;
pub use crdt::{CrdtStore, LwwRegister, OrSet};
pub use delegation::MeshDelegation;
