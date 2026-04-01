//! CRDT-based State Replication — conflict-free replicated data types
//! for mesh state synchronization.
//!
//! Instead of consensus (Raft/Paxos), we use CRDTs which guarantee
//! eventual consistency without coordination. This is the right choice
//! for agent mesh state because:
//! 1. Agent registrations are commutative (register A, register B = register B, register A)
//! 2. We tolerate stale reads (discovery can work with slightly old data)
//! 3. Availability > consistency for mesh operations
//!
//! Implemented CRDTs:
//! - GCounter: grow-only counter (total tasks completed across mesh)
//! - LWWRegister: last-writer-wins register (agent status, capabilities)
//! - ORSet: observed-remove set (set of active agents)
//! - MeshState: composite CRDT wrapping all mesh state

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

// ── G-Counter (grow-only counter) ───────────────────────────────────

/// Grow-only counter — each node maintains its own count.
/// Total = Σ counts[node]. Merge = max per node.
///
/// Invariant: counts are monotonically non-decreasing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GCounter {
    counts: BTreeMap<String, u64>,
}

impl GCounter {
    pub fn new() -> Self {
        Self {
            counts: BTreeMap::new(),
        }
    }

    /// Increment this node's count.
    pub fn increment(&mut self, node_id: &str) {
        *self.counts.entry(node_id.to_string()).or_insert(0) += 1;
    }

    /// Increment by a specific amount.
    pub fn increment_by(&mut self, node_id: &str, n: u64) {
        *self.counts.entry(node_id.to_string()).or_insert(0) += n;
    }

    /// Get the total count across all nodes.
    pub fn value(&self) -> u64 {
        self.counts.values().sum()
    }

    /// Get a specific node's count.
    pub fn node_count(&self, node_id: &str) -> u64 {
        self.counts.get(node_id).copied().unwrap_or(0)
    }

    /// Merge with another GCounter (take max per node).
    pub fn merge(&mut self, other: &GCounter) {
        for (node, &count) in &other.counts {
            let entry = self.counts.entry(node.clone()).or_insert(0);
            *entry = (*entry).max(count);
        }
    }
}

impl Default for GCounter {
    fn default() -> Self {
        Self::new()
    }
}

// ── LWW-Register (last-writer-wins register) ───────────────────────

/// Last-writer-wins register — stores a value with a timestamp.
/// On conflict, the value with the higher timestamp wins.
/// If timestamps tie, we use lexicographic ordering on the node ID
/// (deterministic tiebreaker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LWWRegister<T: Clone + Serialize> {
    value: T,
    /// Lamport timestamp (logical clock).
    timestamp: u64,
    /// Node that last wrote (tiebreaker).
    writer: String,
}

impl<T: Clone + Serialize + Default> LWWRegister<T> {
    pub fn new(value: T, writer: &str) -> Self {
        Self {
            value,
            timestamp: 0,
            writer: writer.to_string(),
        }
    }

    /// Set a new value with an incremented timestamp.
    pub fn set(&mut self, value: T, writer: &str) {
        self.timestamp += 1;
        self.value = value;
        self.writer = writer.to_string();
    }

    /// Get the current value.
    pub fn get(&self) -> &T {
        &self.value
    }

    /// Get the logical timestamp.
    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    /// Merge: keep the value with the higher timestamp.
    pub fn merge(&mut self, other: &LWWRegister<T>) {
        if other.timestamp > self.timestamp
            || (other.timestamp == self.timestamp && other.writer > self.writer)
        {
            self.value = other.value.clone();
            self.timestamp = other.timestamp;
            self.writer = other.writer.clone();
        }
    }
}

// ── OR-Set (observed-remove set) ────────────────────────────────────

/// Observed-remove set — supports both add and remove without conflicts.
///
/// Each element is tagged with a unique token (node_id + sequence number).
/// Add creates a new tag. Remove removes all observed tags for that element.
/// An element is in the set iff it has at least one tag.
///
/// This avoids the "add-remove concurrency" problem:
/// - If add and remove happen concurrently, the add wins (desired behavior
///   for agent registration: if someone adds an agent while another removes
///   a stale entry, the fresh add should win).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ORSet<T: Clone + Ord + Serialize> {
    /// element → set of (node_id, sequence_number) tags.
    entries: BTreeMap<T, BTreeSet<(String, u64)>>,
    /// Per-node sequence counter.
    seq: BTreeMap<String, u64>,
}

impl<T: Clone + Ord + Serialize + for<'de> serde::Deserialize<'de>> ORSet<T> {
    pub fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            seq: BTreeMap::new(),
        }
    }

    /// Add an element, tagged with this node.
    pub fn add(&mut self, element: T, node_id: &str) {
        let seq = self.seq.entry(node_id.to_string()).or_insert(0);
        *seq += 1;
        let tag = (node_id.to_string(), *seq);
        self.entries.entry(element).or_default().insert(tag);
    }

    /// Remove an element (removes all observed tags).
    pub fn remove(&mut self, element: &T) {
        self.entries.remove(element);
    }

    /// Check if an element is in the set.
    pub fn contains(&self, element: &T) -> bool {
        self.entries
            .get(element)
            .map(|tags| !tags.is_empty())
            .unwrap_or(false)
    }

    /// Get all elements currently in the set.
    pub fn elements(&self) -> Vec<&T> {
        self.entries
            .iter()
            .filter(|(_, tags)| !tags.is_empty())
            .map(|(elem, _)| elem)
            .collect()
    }

    /// Number of elements in the set.
    pub fn len(&self) -> usize {
        self.entries
            .iter()
            .filter(|(_, tags)| !tags.is_empty())
            .count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Merge with another ORSet (union of tags per element).
    pub fn merge(&mut self, other: &ORSet<T>) {
        for (elem, other_tags) in &other.entries {
            let tags = self.entries.entry(elem.clone()).or_default();
            for tag in other_tags {
                tags.insert(tag.clone());
            }
        }
        for (node, &other_seq) in &other.seq {
            let seq = self.seq.entry(node.clone()).or_insert(0);
            *seq = (*seq).max(other_seq);
        }
    }
}

impl<T: Clone + Ord + Serialize + for<'de> serde::Deserialize<'de>> Default for ORSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

// ── Composite mesh state CRDT ───────────────────────────────────────

/// The replicated state of the entire mesh.
/// Composed of individual CRDTs for different aspects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshState {
    /// Set of active agent IDs (ORSet for safe add/remove).
    pub active_agents: ORSet<String>,
    /// Total tasks completed across the mesh (GCounter).
    pub tasks_completed: GCounter,
    /// Total tasks failed across the mesh (GCounter).
    pub tasks_failed: GCounter,
    /// Per-agent capability hashes (LWW for latest capabilities).
    pub agent_capabilities: BTreeMap<String, LWWRegister<String>>,
    /// Mesh-wide version vector for detecting sync staleness.
    pub version: BTreeMap<String, u64>,
}

impl MeshState {
    pub fn new() -> Self {
        Self {
            active_agents: ORSet::new(),
            tasks_completed: GCounter::new(),
            tasks_failed: GCounter::new(),
            agent_capabilities: BTreeMap::new(),
            version: BTreeMap::new(),
        }
    }

    /// Register an agent in the mesh state.
    pub fn register_agent(
        &mut self,
        agent_id: &str,
        capability_hash: &str,
        node_id: &str,
    ) {
        self.active_agents.add(agent_id.to_string(), node_id);

        let reg = self
            .agent_capabilities
            .entry(agent_id.to_string())
            .or_insert_with(|| LWWRegister::new(String::new(), node_id));
        reg.set(capability_hash.to_string(), node_id);

        self.bump_version(node_id);
    }

    /// Remove an agent from the mesh state.
    pub fn deregister_agent(&mut self, agent_id: &str, node_id: &str) {
        self.active_agents.remove(&agent_id.to_string());
        self.bump_version(node_id);
    }

    /// Record a task completion.
    pub fn record_task_completed(&mut self, node_id: &str) {
        self.tasks_completed.increment(node_id);
        self.bump_version(node_id);
    }

    /// Record a task failure.
    pub fn record_task_failed(&mut self, node_id: &str) {
        self.tasks_failed.increment(node_id);
        self.bump_version(node_id);
    }

    /// Merge with another MeshState (component-wise CRDT merge).
    pub fn merge(&mut self, other: &MeshState) {
        self.active_agents.merge(&other.active_agents);
        self.tasks_completed.merge(&other.tasks_completed);
        self.tasks_failed.merge(&other.tasks_failed);

        for (agent_id, other_reg) in &other.agent_capabilities {
            let reg = self
                .agent_capabilities
                .entry(agent_id.clone())
                .or_insert_with(|| LWWRegister::new(String::new(), ""));
            reg.merge(other_reg);
        }

        for (node, &version) in &other.version {
            let v = self.version.entry(node.clone()).or_insert(0);
            *v = (*v).max(version);
        }
    }

    /// Bump version vector for a node.
    fn bump_version(&mut self, node_id: &str) {
        *self.version.entry(node_id.to_string()).or_insert(0) += 1;
    }

    /// Check if this state is ahead of another (for sync decisions).
    pub fn is_ahead_of(&self, other: &MeshState) -> bool {
        for (node, &version) in &self.version {
            let other_ver = other.version.get(node).copied().unwrap_or(0);
            if version > other_ver {
                return true;
            }
        }
        false
    }

    /// Summary statistics.
    pub fn summary(&self) -> MeshStateSummary {
        MeshStateSummary {
            active_agent_count: self.active_agents.len(),
            total_tasks_completed: self.tasks_completed.value(),
            total_tasks_failed: self.tasks_failed.value(),
            version_sum: self.version.values().sum(),
        }
    }
}

impl Default for MeshState {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of mesh state for display.
#[derive(Debug, Clone)]
pub struct MeshStateSummary {
    pub active_agent_count: usize,
    pub total_tasks_completed: u64,
    pub total_tasks_failed: u64,
    pub version_sum: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gcounter_merge() {
        let mut c1 = GCounter::new();
        let mut c2 = GCounter::new();

        c1.increment("node-a");
        c1.increment("node-a");
        c2.increment("node-b");
        c2.increment("node-b");
        c2.increment("node-b");

        c1.merge(&c2);
        assert_eq!(c1.value(), 5); // 2 + 3
        assert_eq!(c1.node_count("node-a"), 2);
        assert_eq!(c1.node_count("node-b"), 3);
    }

    #[test]
    fn test_gcounter_merge_idempotent() {
        let mut c1 = GCounter::new();
        let c2 = GCounter::new();

        c1.increment("a");
        c1.merge(&c2);
        c1.merge(&c2); // Idempotent
        assert_eq!(c1.value(), 1);
    }

    #[test]
    fn test_lww_merge_timestamp_wins() {
        let mut r1: LWWRegister<String> = LWWRegister::new("initial".into(), "node-a");
        let mut r2: LWWRegister<String> = LWWRegister::new("initial".into(), "node-b");

        r1.set("value-a".into(), "node-a"); // ts=1
        r2.set("value-b-1".into(), "node-b"); // ts=1
        r2.set("value-b-2".into(), "node-b"); // ts=2

        r1.merge(&r2); // r2 has higher timestamp
        assert_eq!(r1.get(), "value-b-2");
    }

    #[test]
    fn test_orset_add_remove() {
        let mut set = ORSet::new();
        set.add("agent-1".to_string(), "node-a");
        set.add("agent-2".to_string(), "node-a");

        assert!(set.contains(&"agent-1".to_string()));
        assert_eq!(set.len(), 2);

        set.remove(&"agent-1".to_string());
        assert!(!set.contains(&"agent-1".to_string()));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_orset_concurrent_add_remove() {
        // Simulates add on node-a and remove on node-b happening concurrently.
        // The add should win because the remove only removes observed tags.
        let mut set_a = ORSet::new();
        let mut set_b = ORSet::new();

        // Both start with agent-1
        set_a.add("agent-1".to_string(), "shared");
        set_b.merge(&set_a);

        // Node B removes agent-1
        set_b.remove(&"agent-1".to_string());
        assert!(!set_b.contains(&"agent-1".to_string()));

        // Node A concurrently adds agent-1 again (new tag)
        set_a.add("agent-1".to_string(), "node-a");

        // Merge: A's new add tag survives B's remove
        set_b.merge(&set_a);
        assert!(
            set_b.contains(&"agent-1".to_string()),
            "Concurrent add should win over remove"
        );
    }

    #[test]
    fn test_mesh_state_merge() {
        let mut s1 = MeshState::new();
        let mut s2 = MeshState::new();

        s1.register_agent("agent-a", "cap-hash-a", "node-1");
        s1.record_task_completed("node-1");
        s1.record_task_completed("node-1");

        s2.register_agent("agent-b", "cap-hash-b", "node-2");
        s2.record_task_completed("node-2");
        s2.record_task_failed("node-2");

        s1.merge(&s2);

        assert_eq!(s1.active_agents.len(), 2);
        assert_eq!(s1.tasks_completed.value(), 3); // 2 + 1
        assert_eq!(s1.tasks_failed.value(), 1);
    }

    #[test]
    fn test_mesh_state_merge_commutative() {
        let mut s1 = MeshState::new();
        let mut s2 = MeshState::new();

        s1.register_agent("a", "h", "n1");
        s2.register_agent("b", "h", "n2");

        let mut result_1_2 = s1.clone();
        result_1_2.merge(&s2);

        let mut result_2_1 = s2.clone();
        result_2_1.merge(&s1);

        // Both orders should produce the same active agents
        assert_eq!(result_1_2.active_agents.len(), result_2_1.active_agents.len());
        assert_eq!(
            result_1_2.tasks_completed.value(),
            result_2_1.tasks_completed.value()
        );
    }
}
