//! Pipit Agent Mesh — Multi-Agent Discovery, Negotiation, and Delegation
//!
//! Enables pipit agents to discover each other's capabilities, negotiate
//! typed communication contracts, and delegate sub-tasks.
//!
//! ## Architecture
//! ```text
//! Stage 1: Registry + Negotiation (in-process discovery)
//! Stage 2: Transport + Failure Detection (network layer)
//! Stage 3: Replication + Routing (distributed state + policy)
//! ```

pub mod registry;
pub mod negotiation;
pub mod transport;
pub mod failure;
pub mod partition;
pub mod replication;
pub mod routing;

pub use registry::{AgentCapability, AgentDescriptor, MeshRegistry};
pub use negotiation::{NegotiationProtocol, SchemaProposal, NegotiationResult};
pub use transport::{MeshEnvelope, MeshMessageType, MeshTransport, TcpTransport, InProcessTransport};
pub use failure::{PhiAccrualDetector, PhiAccrualConfig, NodeLiveness};
pub use replication::{MeshState, GCounter, LWWRegister, ORSet};
pub use routing::{PolicyRouter, RoutingRequest, RoutingDecision, Tenant, RoutingRule};
