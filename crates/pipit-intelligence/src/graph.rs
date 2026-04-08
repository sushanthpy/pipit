use crate::tags::{FileTag, TagKind};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// A reference graph: nodes are files, edges represent cross-file references.
pub struct ReferenceGraph {
    pub graph: DiGraph<PathBuf, u32>,
    pub node_indices: HashMap<PathBuf, NodeIndex>,
    pub tags: Vec<FileTag>,
}

impl ReferenceGraph {
    /// Build a reference graph from extracted tags.
    pub fn build(tags: &[FileTag]) -> Self {
        // Index: symbol_name → files where it's defined
        let mut definitions: HashMap<&str, Vec<&Path>> = HashMap::new();
        // Index: symbol_name → files where it's referenced
        let mut references: HashMap<&str, Vec<&Path>> = HashMap::new();

        for tag in tags {
            match tag.kind {
                TagKind::Definition => {
                    definitions
                        .entry(&tag.name)
                        .or_default()
                        .push(&tag.rel_path);
                }
                TagKind::Reference => {
                    references.entry(&tag.name).or_default().push(&tag.rel_path);
                }
            }
        }

        let mut graph = DiGraph::<PathBuf, u32>::new();
        let mut node_indices: HashMap<PathBuf, NodeIndex> = HashMap::new();

        // Ensure all files are nodes
        let all_files: HashSet<&Path> = tags.iter().map(|t| t.rel_path.as_path()).collect();
        for file in &all_files {
            let idx = graph.add_node(file.to_path_buf());
            node_indices.insert(file.to_path_buf(), idx);
        }

        // Add edges: ref_file → def_file
        for (symbol, ref_files) in &references {
            if let Some(def_files) = definitions.get(symbol) {
                for ref_file in ref_files {
                    for def_file in def_files {
                        if ref_file != def_file {
                            let from = node_indices[*ref_file];
                            let to = node_indices[*def_file];
                            if let Some(edge) = graph.find_edge(from, to) {
                                *graph.edge_weight_mut(edge).unwrap() += 1;
                            } else {
                                graph.add_edge(from, to, 1);
                            }
                        }
                    }
                }
            }
        }

        ReferenceGraph {
            graph,
            node_indices,
            tags: tags.to_vec(),
        }
    }

    /// Compute PageRank scores — Fix #16: weight-aware distribution.
    pub fn page_rank(&self) -> HashMap<PathBuf, f64> {
        let n = self.graph.node_count();
        if n == 0 {
            return HashMap::new();
        }

        /// Standard PageRank damping factor (probability of following a link vs. random jump).
        const PAGERANK_DAMPING: f64 = 0.85;
        /// Number of power-iteration rounds; 50 is sufficient for convergence on typical repos.
        const PAGERANK_ITERATIONS: usize = 50;

        let damping = PAGERANK_DAMPING;
        let iterations = PAGERANK_ITERATIONS;
        let initial = 1.0 / n as f64;
        let mut ranks: Vec<f64> = vec![initial; n];

        for _ in 0..iterations {
            let mut new_ranks = vec![(1.0 - damping) / n as f64; n];
            for node in self.graph.node_indices() {
                // Fix #16: Compute total outgoing weight for proportional distribution
                let total_weight: u32 = self
                    .graph
                    .edges_directed(node, petgraph::Direction::Outgoing)
                    .map(|e| *e.weight())
                    .sum();
                if total_weight > 0 {
                    for edge in self
                        .graph
                        .edges_directed(node, petgraph::Direction::Outgoing)
                    {
                        let weight = *edge.weight() as f64;
                        let share = ranks[node.index()] * weight / total_weight as f64;
                        new_ranks[edge.target().index()] += damping * share;
                    }
                }
            }
            ranks = new_ranks;
        }

        self.graph
            .node_indices()
            .map(|idx| (self.graph[idx].clone(), ranks[idx.index()]))
            .collect()
    }

    /// Get definitions in a specific file.
    pub fn definitions_in(&self, path: &Path) -> Vec<&FileTag> {
        self.tags
            .iter()
            .filter(|t| t.rel_path == path && t.kind == TagKind::Definition)
            .collect()
    }

    /// Update graph with new tags for changed files.
    pub fn update_files(&mut self, changed: &HashSet<PathBuf>, new_tags: &[FileTag]) {
        // Remove old tags for changed files
        self.tags.retain(|t| !changed.contains(&t.rel_path));

        // Add new tags
        self.tags.extend(new_tags.iter().cloned());

        // Rebuild graph (could be optimized for incremental)
        *self = Self::build(&self.tags);
    }
}
