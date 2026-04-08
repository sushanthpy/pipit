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

pub mod failure;
pub mod negotiation;
pub mod partition;
pub mod registry;
pub mod replication;
pub mod routing;
pub mod transport;

pub use failure::{NodeLiveness, PhiAccrualConfig, PhiAccrualDetector};
pub use negotiation::{NegotiationProtocol, NegotiationResult, SchemaProposal};
pub use registry::{AgentCapability, AgentDescriptor, MeshRegistry};
pub use replication::{GCounter, LWWRegister, MeshState, ORSet};
pub use routing::{PolicyRouter, RoutingDecision, RoutingRequest, RoutingRule, Tenant};
pub use transport::{
    InProcessTransport, MeshEnvelope, MeshMessageType, MeshTransport, TcpTransport,
};
