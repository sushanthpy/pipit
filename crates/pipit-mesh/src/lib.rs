//! pipit-mesh: Distributed multi-agent coordination via P2P mesh networking.
//!
//! Consolidated mesh crate — single distributed-coordination surface.
//! Combines SWIM gossip, CRDT replication, phi-accrual failure detection,
//! contract negotiation, capability-based routing, and transport (in-process + TCP).
//!
//! ## Architecture
//! ```text
//! ┌─ MeshDaemon ─────────────────────────┐
//! │  SwimProtocol  ←  gossip messages     │
//! │  NodeRegistry  ←  capability index    │
//! │  CrdtStore     ←  shared state        │
//! │  DelegationEngine ← task routing      │
//! │  PhiAccrualDetector ← failure detect  │
//! │  PolicyRouter  ←  tenant routing      │
//! └──────────────────────────────────────┘
//! ```

// ── Local modules (original pipit-mesh) ──
pub mod crdt;
pub mod delegation;
pub mod node;
pub mod swim;

// ── Merged modules (from pipit-agent-mesh) ──
pub mod failure;
pub mod negotiation;
pub mod partition;
pub mod registry;
pub mod replication;
pub mod routing;
pub mod transport;

// ── Public API ──
pub use crdt::{CrdtStore, LwwRegister, OrSet};
pub use delegation::MeshDelegation;
pub use node::{MeshDaemon, NodeDescriptor, NodeId};
pub use swim::SwimProtocol;
pub use failure::{NodeLiveness, PhiAccrualConfig, PhiAccrualDetector};
pub use negotiation::{NegotiationProtocol, NegotiationResult};
pub use registry::{AgentCapability, AgentDescriptor, MeshRegistry};
pub use replication::{GCounter, MeshState};
pub use routing::{PolicyRouter, RoutingDecision, RoutingRequest};
pub use transport::{InProcessTransport, MeshTransport, TcpTransport};

// ═══════════════════════════════════════════════════════════════════════════
//  Integration tests — verify all merged modules compose correctly
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod integration_tests {
    use super::*;

    /// Verify that all public re-exports from both original crates are accessible.
    #[test]
    fn all_public_types_constructible() {
        // CRDT types (original pipit-mesh)
        let _store = CrdtStore::new();
        let _lww = LwwRegister::new("value".to_string(), "node-1");
        let mut orset: OrSet<String> = OrSet::new();
        orset.add("item".to_string(), "node-1");
        assert!(orset.contains(&"item".to_string()));

        // Failure detection (merged from pipit-agent-mesh)
        let config = PhiAccrualConfig::default();
        let _detector = PhiAccrualDetector::new(config);

        // Registry (merged)
        let _registry = MeshRegistry::new();

        // Replication (merged)
        let _counter = GCounter::new();

        // Routing (merged)
        let _router = PolicyRouter::new();
    }

    /// Test CRDT LWW register merge — later timestamp wins.
    #[test]
    fn crdt_lww_merge_resolves_by_timestamp() {
        let mut reg_a = LwwRegister::new("old_value".to_string(), "node-a");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let reg_b = LwwRegister::new("new_value".to_string(), "node-b");

        reg_a.merge(&reg_b);
        assert_eq!(reg_a.value, "new_value");
    }

    /// Test OR-Set concurrent add/remove across nodes.
    #[test]
    fn crdt_orset_concurrent_operations() {
        let mut set_a: OrSet<String> = OrSet::new();
        let mut set_b: OrSet<String> = OrSet::new();

        set_a.add("item1".to_string(), "node-a");
        set_a.add("item2".to_string(), "node-a");
        set_b.add("item2".to_string(), "node-b");
        set_b.add("item3".to_string(), "node-b");

        set_a.merge(&set_b);
        assert!(set_a.contains(&"item1".to_string()));
        assert!(set_a.contains(&"item2".to_string()));
        assert!(set_a.contains(&"item3".to_string()));
    }

    /// Test CrdtStore merge across two nodes.
    #[test]
    fn crdt_store_merge_remote() {
        let mut store_a = CrdtStore::new();
        let mut store_b = CrdtStore::new();

        store_a.file_states.insert(
            "src/lib.rs".to_string(),
            LwwRegister::new("content_v1".to_string(), "node-a"),
        );
        store_b.file_states.insert(
            "src/main.rs".to_string(),
            LwwRegister::new("main_content".to_string(), "node-b"),
        );

        store_a.merge_remote(&store_b);

        assert!(store_a.file_states.contains_key("src/lib.rs"));
        assert!(store_a.file_states.contains_key("src/main.rs"));
    }

    /// Test registry register + discover compose.
    #[test]
    fn registry_register_and_discover() {
        let registry = MeshRegistry::new();
        let descriptor = AgentDescriptor {
            id: "agent-1".to_string(),
            name: "explore".to_string(),
            tools: ["read_file", "grep"].iter().map(|s| s.to_string()).collect(),
            languages: ["rust"].iter().map(|s| s.to_string()).collect(),
            projects: Default::default(),
            tags: ["readonly"].iter().map(|s| s.to_string()).collect(),
            endpoint: "local".to_string(),
            last_seen: chrono::Utc::now(),
        };
        registry.register(descriptor);
        assert_eq!(registry.agent_count(), 1);

        let query = AgentCapability {
            required_tools: ["read_file"].iter().map(|s| s.to_string()).collect(),
            required_languages: Default::default(),
            required_tags: Default::default(),
        };
        let results = registry.discover(&query);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.name, "explore");
    }

    /// Test phi-accrual detector signals liveness after heartbeats.
    #[test]
    fn failure_detector_heartbeat_liveness() {
        let config = PhiAccrualConfig::default();
        let mut detector = PhiAccrualDetector::new(config);

        detector.heartbeat("node-1");
        std::thread::sleep(std::time::Duration::from_millis(10));
        detector.heartbeat("node-1");

        assert!(detector.is_alive("node-1"));
        assert_eq!(detector.node_count(), 1);
    }

    /// Test GCounter increment and merge across nodes.
    #[test]
    fn gcounter_multi_node_merge() {
        let mut counter_a = GCounter::new();
        let mut counter_b = GCounter::new();

        counter_a.increment_by("node-a", 5);
        counter_b.increment_by("node-b", 3);

        counter_a.merge(&counter_b);
        assert_eq!(counter_a.value(), 8);

        // Merge is idempotent
        counter_a.merge(&counter_b);
        assert_eq!(counter_a.value(), 8);
    }

    /// Test MeshState register/deregister/task tracking.
    #[test]
    fn mesh_state_agent_lifecycle() {
        let mut state = MeshState::new();

        state.register_agent("agent-1", "caps_hash_1", "node-a");
        state.record_task_completed("node-a");
        state.record_task_completed("node-a");
        state.record_task_failed("node-a");

        let summary = state.summary();
        assert_eq!(summary.active_agent_count, 1);
        assert_eq!(summary.total_tasks_completed, 2);
        assert_eq!(summary.total_tasks_failed, 1);

        state.deregister_agent("agent-1", "node-a");
        let summary = state.summary();
        assert_eq!(summary.active_agent_count, 0);
    }

    /// Test MeshState merge across two nodes.
    #[test]
    fn mesh_state_merge() {
        let mut state_a = MeshState::new();
        let mut state_b = MeshState::new();

        state_a.register_agent("agent-1", "caps_1", "node-a");
        state_a.record_task_completed("node-a");
        state_b.register_agent("agent-2", "caps_2", "node-b");
        state_b.record_task_completed("node-b");

        state_a.merge(&state_b);
        let summary = state_a.summary();
        assert_eq!(summary.active_agent_count, 2);
        assert_eq!(summary.total_tasks_completed, 2);
    }

    /// Test MeshEnvelope wire format round-trip (length-prefixed).
    #[test]
    fn envelope_wire_roundtrip() {
        let envelope = transport::MeshEnvelope::new(
            "node-a",
            "node-b",
            transport::MeshMessageType::Ping,
            serde_json::json!({"seq": 1}),
        );

        let wire = envelope.to_wire().unwrap();
        // First 4 bytes are BE length prefix
        assert!(wire.len() > 4);
        let json_bytes = &wire[4..];
        let decoded = transport::MeshEnvelope::from_json(json_bytes).unwrap();
        assert_eq!(decoded.from, "node-a");
        assert_eq!(decoded.to, "node-b");
    }

    /// Test envelope reply preserves correlation.
    #[test]
    fn envelope_reply_preserves_correlation() {
        let original = transport::MeshEnvelope::new(
            "node-a",
            "node-b",
            transport::MeshMessageType::Ping,
            serde_json::json!({}),
        );
        let reply = original.reply(
            transport::MeshMessageType::Pong,
            serde_json::json!({"status": "ok"}),
        );

        assert_eq!(reply.from, "node-b");
        assert_eq!(reply.to, "node-a");
        assert_eq!(reply.correlation_id, Some(original.message_id.clone()));
    }

    /// Test SWIM protocol starts and can handle messages.
    #[test]
    fn swim_protocol_initial_state() {
        let config = swim::SwimConfig::default();
        let protocol = SwimProtocol::new("node-test".into(), config);
        assert_eq!(protocol.local_id, "node-test");
    }
}
