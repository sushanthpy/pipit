//! CRDT (Conflict-Free Replicated Data Types) for distributed shared state.
//!
//! Implements:
//! - Last-Writer-Wins Register (LWW-Register) for file-level state
//! - OR-Set (Observed-Remove Set) for shared context windows
//! - Hybrid Logical Clock (HLC) for causal ordering
//!
//! All operations are commutative: merge(s₁, s₂) = merge(s₂, s₁)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Hybrid Logical Clock timestamp for causal ordering.
/// HLC: max(local_clock, msg_clock) + 1
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HlcTimestamp {
    pub wall_ms: u64,
    pub counter: u32,
    pub node_id_hash: u32,
}

impl HlcTimestamp {
    pub fn now(node_id: &str) -> Self {
        let wall_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        Self {
            wall_ms,
            counter: 0,
            node_id_hash: Self::hash_node(node_id),
        }
    }

    pub fn update(&self, remote: &HlcTimestamp, node_id: &str) -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let max_wall = now_ms.max(self.wall_ms).max(remote.wall_ms);
        let counter = if max_wall == self.wall_ms && max_wall == remote.wall_ms {
            self.counter.max(remote.counter) + 1
        } else if max_wall == self.wall_ms {
            self.counter + 1
        } else if max_wall == remote.wall_ms {
            remote.counter + 1
        } else {
            0
        };
        Self {
            wall_ms: max_wall,
            counter,
            node_id_hash: Self::hash_node(node_id),
        }
    }

    fn hash_node(node_id: &str) -> u32 {
        let mut hash: u32 = 5381;
        for byte in node_id.bytes() {
            hash = hash.wrapping_mul(33).wrapping_add(byte as u32);
        }
        hash
    }
}

/// Last-Writer-Wins Register — stores a single value with a timestamp.
/// On conflict, the value with the highest HLC timestamp wins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LwwRegister<V: Clone> {
    pub value: V,
    pub timestamp: HlcTimestamp,
}

impl<V: Clone> LwwRegister<V> {
    pub fn new(value: V, node_id: &str) -> Self {
        Self {
            value,
            timestamp: HlcTimestamp::now(node_id),
        }
    }

    /// Merge with a remote register. Higher timestamp wins.
    pub fn merge(&mut self, remote: &LwwRegister<V>) {
        if remote.timestamp > self.timestamp {
            self.value = remote.value.clone();
            self.timestamp = remote.timestamp;
        }
    }

    pub fn set(&mut self, value: V, node_id: &str) {
        self.value = value;
        self.timestamp = HlcTimestamp::now(node_id);
    }
}

/// OR-Set (Observed-Remove Set) — supports concurrent add/remove without conflicts.
/// Each element has a unique tag (node_id + counter). Remove only removes observed tags.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrSet<T: Clone + Eq + std::hash::Hash> {
    /// Elements with their unique tags: element → set of (node_id, counter)
    elements: HashMap<T, HashSet<(String, u64)>>,
    /// Tombstones for removed tags
    tombstones: HashSet<(String, u64)>,
    /// Per-node counters for generating unique tags
    counters: HashMap<String, u64>,
}

impl<T: Clone + Eq + std::hash::Hash + Serialize + for<'de> Deserialize<'de>> OrSet<T> {
    pub fn new() -> Self {
        Self {
            elements: HashMap::new(),
            tombstones: HashSet::new(),
            counters: HashMap::new(),
        }
    }

    /// Add an element with a unique tag.
    pub fn add(&mut self, element: T, node_id: &str) {
        let counter = self.counters.entry(node_id.to_string()).or_insert(0);
        *counter += 1;
        let tag = (node_id.to_string(), *counter);
        self.elements.entry(element).or_default().insert(tag);
    }

    /// Remove an element by removing all its currently observed tags.
    pub fn remove(&mut self, element: &T) {
        if let Some(tags) = self.elements.remove(element) {
            for tag in tags {
                self.tombstones.insert(tag);
            }
        }
    }

    /// Check if an element is in the set.
    pub fn contains(&self, element: &T) -> bool {
        self.elements
            .get(element)
            .map(|tags| !tags.is_empty())
            .unwrap_or(false)
    }

    /// Get all elements currently in the set.
    pub fn elements(&self) -> Vec<&T> {
        self.elements
            .iter()
            .filter(|(_, tags)| !tags.is_empty())
            .map(|(elem, _)| elem)
            .collect()
    }

    /// Merge with a remote OR-Set (commutative, associative, idempotent).
    pub fn merge(&mut self, remote: &OrSet<T>) {
        // Add all remote elements not in our tombstones
        for (elem, remote_tags) in &remote.elements {
            let local_tags = self.elements.entry(elem.clone()).or_default();
            for tag in remote_tags {
                if !self.tombstones.contains(tag) {
                    local_tags.insert(tag.clone());
                }
            }
        }
        // Add all remote tombstones
        for tag in &remote.tombstones {
            self.tombstones.insert(tag.clone());
            // Remove tombstoned tags from elements
            for tags in self.elements.values_mut() {
                tags.remove(tag);
            }
        }
        // Merge counters (take max)
        for (node, &counter) in &remote.counters {
            let local = self.counters.entry(node.clone()).or_insert(0);
            *local = (*local).max(counter);
        }
    }
}

/// Top-level CRDT store for the mesh — holds all shared state.
#[derive(Debug)]
pub struct CrdtStore {
    /// file path → current content hash (LWW register)
    pub file_states: HashMap<String, LwwRegister<String>>,
    /// shared context: set of file paths currently in context
    pub shared_context: OrSet<String>,
    /// task assignments: task_id → assigned_node_id
    pub task_assignments: HashMap<String, LwwRegister<String>>,
}

impl CrdtStore {
    pub fn new() -> Self {
        Self {
            file_states: HashMap::new(),
            shared_context: OrSet::new(),
            task_assignments: HashMap::new(),
        }
    }

    /// Merge all state from a remote node.
    pub fn merge_remote(&mut self, remote: &CrdtStore) {
        for (path, remote_reg) in &remote.file_states {
            match self.file_states.get_mut(path) {
                Some(local) => local.merge(remote_reg),
                None => {
                    self.file_states.insert(path.clone(), remote_reg.clone());
                }
            }
        }
        self.shared_context.merge(&remote.shared_context);
        for (tid, remote_reg) in &remote.task_assignments {
            match self.task_assignments.get_mut(tid) {
                Some(local) => local.merge(remote_reg),
                None => {
                    self.task_assignments
                        .insert(tid.clone(), remote_reg.clone());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hlc_ordering() {
        let t1 = HlcTimestamp::now("node-a");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let t2 = HlcTimestamp::now("node-b");
        assert!(t2 > t1);
    }

    #[test]
    fn test_lww_register_merge() {
        let mut r1 = LwwRegister::new("old".to_string(), "node-a");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let r2 = LwwRegister::new("new".to_string(), "node-b");
        r1.merge(&r2);
        assert_eq!(r1.value, "new");
    }

    #[test]
    fn test_or_set_add_remove() {
        let mut set: OrSet<String> = OrSet::new();
        set.add("file.rs".to_string(), "node-a");
        assert!(set.contains(&"file.rs".to_string()));
        set.remove(&"file.rs".to_string());
        assert!(!set.contains(&"file.rs".to_string()));
    }

    #[test]
    fn test_or_set_concurrent_add_remove_merge() {
        // Node A adds "x", node B adds "x" independently
        let mut set_a: OrSet<String> = OrSet::new();
        let mut set_b: OrSet<String> = OrSet::new();
        set_a.add("x".to_string(), "node-a");
        set_b.add("x".to_string(), "node-b");

        // Node A removes "x" (only removes its own tag)
        set_a.remove(&"x".to_string());

        // Merge: B's add should survive A's remove
        set_a.merge(&set_b);
        assert!(
            set_a.contains(&"x".to_string()),
            "Concurrent add from B should survive A's remove"
        );
    }

    #[test]
    fn test_crdt_store_merge() {
        let mut store_a = CrdtStore::new();
        let mut store_b = CrdtStore::new();

        store_a
            .shared_context
            .add("src/main.rs".to_string(), "node-a");
        store_b
            .shared_context
            .add("src/lib.rs".to_string(), "node-b");

        store_a.merge_remote(&store_b);
        assert!(store_a.shared_context.contains(&"src/main.rs".to_string()));
        assert!(store_a.shared_context.contains(&"src/lib.rs".to_string()));
    }
}
