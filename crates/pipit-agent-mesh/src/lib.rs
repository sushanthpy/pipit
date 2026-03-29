//! Pipit Agent Mesh — Multi-Agent Discovery, Negotiation, and Delegation
//!
//! Enables pipit agents to discover each other's capabilities, negotiate
//! typed communication contracts, and delegate sub-tasks.

pub mod registry;
pub mod negotiation;

pub use registry::{AgentCapability, AgentDescriptor, MeshRegistry};
pub use negotiation::{NegotiationProtocol, SchemaProposal, NegotiationResult};
