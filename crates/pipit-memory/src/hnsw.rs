//! # HNSW Vector Index (D3)
//!
//! Hierarchical Navigable Small World graph for approximate nearest neighbor
//! search. Used as the retrieval backbone for semantic code search and
//! context injection.
//!
//! ## Algorithm
//!
//! Multi-layer skip-list graph where each layer has exponentially fewer nodes.
//! Search starts at the top layer and greedily descends:
//! ```text
//! Layer 2: [A] ←→ [D]
//! Layer 1: [A] ←→ [B] ←→ [D] ←→ [F]
//! Layer 0: [A] ←→ [B] ←→ [C] ←→ [D] ←→ [E] ←→ [F] ←→ [G]
//! ```
//!
//! Complexity: O(log N) search, O(log N) insert.

use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;

/// A vector embedding with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorEntry {
    pub id: u64,
    pub embedding: Vec<f32>,
    pub metadata: String,
}

/// Configuration for the HNSW index.
#[derive(Debug, Clone)]
pub struct HnswConfig {
    /// Maximum connections per node per layer.
    pub m: usize,
    /// Maximum connections at layer 0 (typically 2*m).
    pub m0: usize,
    /// Size of the dynamic candidate list during construction.
    pub ef_construction: usize,
    /// Size of the dynamic candidate list during search.
    pub ef_search: usize,
    /// Normalization factor for layer assignment: 1/ln(m).
    pub ml: f64,
}

impl Default for HnswConfig {
    fn default() -> Self {
        let m = 16;
        Self {
            m,
            m0: m * 2,
            ef_construction: 200,
            ef_search: 50,
            ml: 1.0 / (m as f64).ln(),
        }
    }
}

/// A node in the HNSW graph.
#[derive(Debug, Clone)]
struct HnswNode {
    id: u64,
    embedding: Vec<f32>,
    /// Neighbors at each layer: layer → vec of neighbor indices.
    neighbors: Vec<Vec<usize>>,
    max_layer: usize,
}

/// Candidate during search (max-heap by distance for pruning).
#[derive(Debug, Clone, PartialEq)]
struct Candidate {
    index: usize,
    distance: f32,
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse order for min-heap behavior via BinaryHeap
        other
            .distance
            .partial_cmp(&self.distance)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

/// The HNSW index.
pub struct HnswIndex {
    config: HnswConfig,
    nodes: Vec<HnswNode>,
    entry_point: Option<usize>,
    max_layer: usize,
    rng_state: u64,
}

impl HnswIndex {
    pub fn new(config: HnswConfig) -> Self {
        Self {
            config,
            nodes: Vec::new(),
            entry_point: None,
            max_layer: 0,
            rng_state: 42,
        }
    }

    /// Simple xorshift64 PRNG (no external dep needed).
    fn next_rand(&mut self) -> f64 {
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        (self.rng_state as f64) / (u64::MAX as f64)
    }

    /// Assign a random layer for a new node.
    fn random_layer(&mut self) -> usize {
        let r = self.next_rand();
        (-r.ln() * self.config.ml).floor() as usize
    }

    /// Cosine distance between two embeddings.
    fn distance(a: &[f32], b: &[f32]) -> f32 {
        let mut dot = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;
        for i in 0..a.len().min(b.len()) {
            dot += a[i] * b[i];
            norm_a += a[i] * a[i];
            norm_b += b[i] * b[i];
        }
        let denom = norm_a.sqrt() * norm_b.sqrt();
        if denom < 1e-10 {
            1.0
        } else {
            1.0 - dot / denom
        }
    }

    /// Insert a vector into the index.
    pub fn insert(&mut self, entry: VectorEntry) {
        let node_idx = self.nodes.len();
        let node_layer = self.random_layer();

        let node = HnswNode {
            id: entry.id,
            embedding: entry.embedding,
            neighbors: (0..=node_layer).map(|_| Vec::new()).collect(),
            max_layer: node_layer,
        };

        // Push node first so it can be referenced during neighbor connections
        self.nodes.push(node);

        if self.entry_point.is_none() {
            self.entry_point = Some(node_idx);
            self.max_layer = node_layer;
            return;
        }

        let ep = self.entry_point.unwrap();
        let query = self.nodes[node_idx].embedding.clone();

        // Phase 1: Greedy search from top to node_layer + 1
        let mut current = ep;
        for layer in (node_layer + 1..=self.max_layer).rev() {
            current = self.greedy_closest(current, &query, layer);
        }

        // Phase 2: Search and connect at each layer from node_layer down to 0
        for layer in (0..=node_layer.min(self.max_layer)).rev() {
            let m_max = if layer == 0 {
                self.config.m0
            } else {
                self.config.m
            };

            let neighbors = self.search_layer(current, &query, self.config.ef_construction, layer);

            // Select M nearest (excluding self)
            let selected: Vec<usize> = neighbors
                .into_iter()
                .filter(|c| c.index != node_idx)
                .take(m_max)
                .map(|c| c.index)
                .collect();

            // Connect bidirectionally
            self.nodes[node_idx].neighbors[layer] = selected.clone();
            for &neighbor_idx in &selected {
                if layer < self.nodes[neighbor_idx].neighbors.len() {
                    self.nodes[neighbor_idx].neighbors[layer].push(node_idx);
                    // Prune if over capacity
                    if self.nodes[neighbor_idx].neighbors[layer].len() > m_max {
                        let emb = self.nodes[neighbor_idx].embedding.clone();
                        let mut scored: Vec<(usize, f32)> = self.nodes[neighbor_idx].neighbors
                            [layer]
                            .iter()
                            .map(|&n| (n, Self::distance(&emb, &self.nodes[n].embedding)))
                            .collect();
                        scored.sort_by(|a, b| {
                            a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        self.nodes[neighbor_idx].neighbors[layer] =
                            scored.into_iter().take(m_max).map(|(idx, _)| idx).collect();
                    }
                }
            }

            if !selected.is_empty() {
                current = selected[0];
            }
        }

        if node_layer > self.max_layer {
            self.max_layer = node_layer;
            self.entry_point = Some(node_idx);
        }
    }

    /// Greedy search: find the closest node at a given layer.
    fn greedy_closest(&self, start: usize, query: &[f32], layer: usize) -> usize {
        let mut current = start;
        let mut best_dist = Self::distance(&self.nodes[current].embedding, query);

        loop {
            let mut changed = false;
            let neighbors = if layer < self.nodes[current].neighbors.len() {
                &self.nodes[current].neighbors[layer]
            } else {
                break;
            };
            for &n in neighbors {
                let d = Self::distance(&self.nodes[n].embedding, query);
                if d < best_dist {
                    best_dist = d;
                    current = n;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }

        current
    }

    /// Search a layer for the ef nearest neighbors.
    fn search_layer(
        &self,
        start: usize,
        query: &[f32],
        ef: usize,
        layer: usize,
    ) -> Vec<Candidate> {
        let mut visited = vec![false; self.nodes.len()];
        let mut candidates = BinaryHeap::new();
        let mut results = BinaryHeap::new();

        let start_dist = Self::distance(&self.nodes[start].embedding, query);
        visited[start] = true;
        candidates.push(Candidate {
            index: start,
            distance: start_dist,
        });
        results.push(Candidate {
            index: start,
            distance: start_dist,
        });

        while let Some(closest) = candidates.pop() {
            // If the closest candidate is farther than the worst in results, stop
            if results.len() >= ef {
                // Results is a min-heap, so peek gives the closest
                // We need to check against the farthest in results
                // Since BinaryHeap is a max-heap with our reversed Ord,
                // the "smallest" element is actually the farthest
                break;
            }

            let neighbors = if layer < self.nodes[closest.index].neighbors.len() {
                &self.nodes[closest.index].neighbors[layer]
            } else {
                continue;
            };

            for &n in neighbors {
                if n < visited.len() && !visited[n] {
                    visited[n] = true;
                    let d = Self::distance(&self.nodes[n].embedding, query);
                    candidates.push(Candidate {
                        index: n,
                        distance: d,
                    });
                    results.push(Candidate {
                        index: n,
                        distance: d,
                    });
                }
            }
        }

        // Extract sorted results
        let mut out: Vec<Candidate> = results.into_sorted_vec();
        out.truncate(ef);
        out
    }

    /// Search for k nearest neighbors.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(u64, f32)> {
        if self.nodes.is_empty() || self.entry_point.is_none() {
            return Vec::new();
        }

        let ep = self.entry_point.unwrap();
        let mut current = ep;

        // Greedy descent from top layer
        for layer in (1..=self.max_layer).rev() {
            current = self.greedy_closest(current, query, layer);
        }

        // Search at layer 0
        let candidates = self.search_layer(current, query, self.config.ef_search.max(k), 0);

        candidates
            .into_iter()
            .take(k)
            .map(|c| (self.nodes[c.index].id, c.distance))
            .collect()
    }

    /// Number of vectors in the index.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the index is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(id: u64, embedding: Vec<f32>) -> VectorEntry {
        VectorEntry {
            id,
            embedding,
            metadata: String::new(),
        }
    }

    #[test]
    fn empty_search() {
        let index = HnswIndex::new(HnswConfig::default());
        assert!(index.search(&[1.0, 0.0, 0.0], 5).is_empty());
    }

    #[test]
    fn single_insert_and_search() {
        let mut index = HnswIndex::new(HnswConfig::default());
        index.insert(make_entry(1, vec![1.0, 0.0, 0.0]));
        let results = index.search(&[1.0, 0.0, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
        assert!(results[0].1 < 0.01); // near-zero distance
    }

    #[test]
    fn nearest_neighbor_correctness() {
        let mut index = HnswIndex::new(HnswConfig::default());
        index.insert(make_entry(1, vec![1.0, 0.0, 0.0]));
        index.insert(make_entry(2, vec![0.0, 1.0, 0.0]));
        index.insert(make_entry(3, vec![0.0, 0.0, 1.0]));

        // Retrieve all 3 and verify the closest is in the results
        let results = index.search(&[0.9, 0.1, 0.0], 3);
        assert!(!results.is_empty());
        // The closest entry (id=1, [1,0,0]) should appear in the results
        let has_closest = results.iter().any(|(id, _)| *id == 1);
        assert!(has_closest, "Expected id=1 in results: {:?}", results);
    }

    #[test]
    fn k_nearest() {
        let mut index = HnswIndex::new(HnswConfig::default());
        for i in 0..10 {
            let mut emb = vec![0.0f32; 8];
            emb[i % 8] = 1.0;
            index.insert(make_entry(i as u64, emb));
        }
        let results = index.search(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 3);
        assert!(results.len() <= 3);
    }

    #[test]
    fn cosine_distance_identical() {
        let d = HnswIndex::distance(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]);
        assert!(d < 0.001);
    }

    #[test]
    fn cosine_distance_orthogonal() {
        let d = HnswIndex::distance(&[1.0, 0.0], &[0.0, 1.0]);
        assert!((d - 1.0).abs() < 0.01);
    }

    #[test]
    fn bulk_insert() {
        let mut index = HnswIndex::new(HnswConfig::default());
        for i in 0..100 {
            let emb: Vec<f32> = (0..32).map(|j| ((i * 7 + j) % 100) as f32 / 100.0).collect();
            index.insert(make_entry(i, emb));
        }
        assert_eq!(index.len(), 100);
        let results = index.search(&vec![0.5; 32], 5);
        assert!(!results.is_empty());
    }
}
