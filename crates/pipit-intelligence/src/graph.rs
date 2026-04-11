use crate::tags::{FileTag, TagKind};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet, VecDeque};
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

    /// Compute the blast radius of a set of changed files via BFS over the reference graph.
    ///
    /// Returns affected files mapped to their BFS depth (1 = direct dependent, 2 = transitive, etc).
    /// `max_depth` limits traversal (default: 3). The changed files themselves are not included
    /// in the output.
    pub fn blast_radius(
        &self,
        changed_files: &[PathBuf],
        max_depth: u32,
    ) -> Vec<ImpactedFile> {
        let mut visited: HashSet<NodeIndex> = HashSet::new();
        let mut queue: VecDeque<(NodeIndex, u32)> = VecDeque::new();
        let mut results: Vec<ImpactedFile> = Vec::new();

        // Seed BFS with changed files
        for file in changed_files {
            if let Some(&idx) = self.node_indices.get(file) {
                visited.insert(idx);
                queue.push_back((idx, 0));
            }
        }

        while let Some((node, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }

            // Follow incoming edges: files that *depend on* this node
            for edge in self
                .graph
                .edges_directed(node, petgraph::Direction::Incoming)
            {
                let dependent = edge.source();
                if visited.insert(dependent) {
                    let dep_depth = depth + 1;
                    let path = self.graph[dependent].clone();
                    let edge_weight = *edge.weight();
                    results.push(ImpactedFile {
                        path,
                        depth: dep_depth,
                        edge_weight,
                    });
                    queue.push_back((dependent, dep_depth));
                }
            }
        }

        // Sort by depth (closest first), then by edge weight descending
        results.sort_by(|a, b| a.depth.cmp(&b.depth).then(b.edge_weight.cmp(&a.edge_weight)));
        results
    }

    /// Score the change risk for a set of modified files.
    ///
    /// Produces a per-file risk score in [0.0, 1.0] based on five factors:
    /// 1. **Caller count** — how many files depend on this file (in-degree)
    /// 2. **Cross-module coupling** — whether dependents span multiple top-level directories
    /// 3. **Test file proximity** — whether the file has associated test files among its dependents
    /// 4. **Security sensitivity** — heuristic keyword scan on the file path
    /// 5. **Blast radius size** — total transitive impact at depth 3
    pub fn change_risk(&self, changed_files: &[PathBuf]) -> Vec<ChangeRisk> {
        let mut risks = Vec::with_capacity(changed_files.len());

        for file in changed_files {
            let Some(&idx) = self.node_indices.get(file) else {
                risks.push(ChangeRisk {
                    path: file.clone(),
                    score: 0.0,
                    factors: RiskFactors::default(),
                });
                continue;
            };

            // Factor 1: Caller count (in-degree)
            let callers: Vec<NodeIndex> = self
                .graph
                .edges_directed(idx, petgraph::Direction::Incoming)
                .map(|e| e.source())
                .collect();
            let caller_count = callers.len();
            // Normalize: 0 callers = 0.0, 10+ callers = 1.0
            let caller_score = (caller_count as f64 / 10.0).min(1.0);

            // Factor 2: Cross-module coupling — unique top-level dirs among callers
            let modules: HashSet<&str> = callers
                .iter()
                .filter_map(|&n| {
                    self.graph[n]
                        .components()
                        .next()
                        .and_then(|c| c.as_os_str().to_str())
                })
                .collect();
            // Normalize: 1 module = 0.0, 4+ modules = 1.0
            let coupling_score = if modules.len() <= 1 {
                0.0
            } else {
                ((modules.len() - 1) as f64 / 3.0).min(1.0)
            };

            // Factor 3: Test proximity — do any callers look like test files?
            let has_test_caller = callers.iter().any(|&n| {
                let p = &self.graph[n];
                let s = p.to_string_lossy();
                s.contains("test") || s.contains("spec") || s.ends_with("_test.rs")
            });
            // Inverted: if tests exist, risk is lower
            let test_score = if has_test_caller { 0.2 } else { 1.0 };

            // Factor 4: Security sensitivity — keywords in file path
            let path_str = file.to_string_lossy();
            let security_keywords = [
                "auth", "cred", "secret", "token", "crypto", "password", "session",
                "permission", "security", "key", "cert", "oauth", "jwt",
            ];
            let sec_hits = security_keywords
                .iter()
                .filter(|kw| path_str.contains(**kw))
                .count();
            let security_score = (sec_hits as f64 / 2.0).min(1.0);

            // Factor 5: Blast radius at depth 3
            let blast = self.blast_radius(&[file.clone()], 3);
            let blast_count = blast.len();
            // Normalize: 0 affected = 0.0, 20+ affected = 1.0
            let blast_score = (blast_count as f64 / 20.0).min(1.0);

            // Weighted combination
            let score = caller_score * 0.25
                + coupling_score * 0.20
                + test_score * 0.20
                + security_score * 0.15
                + blast_score * 0.20;

            risks.push(ChangeRisk {
                path: file.clone(),
                score: (score * 100.0).round() / 100.0, // 2 decimal places
                factors: RiskFactors {
                    caller_count,
                    cross_module_count: modules.len(),
                    has_test_coverage: has_test_caller,
                    security_sensitive: sec_hits > 0,
                    blast_radius_size: blast_count,
                },
            });
        }

        // Sort by risk score descending
        risks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        risks
    }
}

/// A file affected by a change, with its BFS distance and edge weight.
#[derive(Debug, Clone)]
pub struct ImpactedFile {
    pub path: PathBuf,
    /// BFS depth from the nearest changed file (1 = direct dependent).
    pub depth: u32,
    /// Weight of the dependency edge (number of symbol references).
    pub edge_weight: u32,
}

/// Risk assessment for a changed file.
#[derive(Debug, Clone)]
pub struct ChangeRisk {
    pub path: PathBuf,
    /// Composite risk score in [0.0, 1.0].
    pub score: f64,
    pub factors: RiskFactors,
}

/// Individual risk factor breakdown.
#[derive(Debug, Clone, Default)]
pub struct RiskFactors {
    pub caller_count: usize,
    pub cross_module_count: usize,
    pub has_test_coverage: bool,
    pub security_sensitive: bool,
    pub blast_radius_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a simple graph: A → B → C (A references B, B references C)
    fn sample_graph() -> ReferenceGraph {
        let tags = vec![
            FileTag { rel_path: PathBuf::from("src/a.rs"), line: 1, name: "Foo".into(), kind: TagKind::Reference },
            FileTag { rel_path: PathBuf::from("src/b.rs"), line: 1, name: "Foo".into(), kind: TagKind::Definition },
            FileTag { rel_path: PathBuf::from("src/b.rs"), line: 5, name: "Bar".into(), kind: TagKind::Reference },
            FileTag { rel_path: PathBuf::from("src/c.rs"), line: 1, name: "Bar".into(), kind: TagKind::Definition },
        ];
        ReferenceGraph::build(&tags)
    }

    #[test]
    fn blast_radius_direct_dependents() {
        let g = sample_graph();
        // Changing c.rs should impact b.rs (depth 1) and a.rs (depth 2)
        let impact = g.blast_radius(&[PathBuf::from("src/c.rs")], 3);
        assert_eq!(impact.len(), 2);
        assert_eq!(impact[0].path, PathBuf::from("src/b.rs"));
        assert_eq!(impact[0].depth, 1);
        assert_eq!(impact[1].path, PathBuf::from("src/a.rs"));
        assert_eq!(impact[1].depth, 2);
    }

    #[test]
    fn blast_radius_depth_limit() {
        let g = sample_graph();
        // With max_depth=1, only direct dependents
        let impact = g.blast_radius(&[PathBuf::from("src/c.rs")], 1);
        assert_eq!(impact.len(), 1);
        assert_eq!(impact[0].path, PathBuf::from("src/b.rs"));
    }

    #[test]
    fn blast_radius_leaf_file_no_impact() {
        let g = sample_graph();
        // a.rs is a leaf — nothing depends on it
        let impact = g.blast_radius(&[PathBuf::from("src/a.rs")], 3);
        assert!(impact.is_empty());
    }

    #[test]
    fn blast_radius_unknown_file() {
        let g = sample_graph();
        let impact = g.blast_radius(&[PathBuf::from("src/unknown.rs")], 3);
        assert!(impact.is_empty());
    }

    #[test]
    fn change_risk_high_for_core_file() {
        // Build a graph where c.rs is referenced by many files
        let mut tags = vec![
            FileTag { rel_path: PathBuf::from("src/c.rs"), line: 1, name: "Core".into(), kind: TagKind::Definition },
        ];
        for i in 0..12 {
            tags.push(FileTag {
                rel_path: PathBuf::from(format!("mod{}/f{}.rs", i % 4, i)),
                line: 1,
                name: "Core".into(),
                kind: TagKind::Reference,
            });
        }
        let g = ReferenceGraph::build(&tags);
        let risks = g.change_risk(&[PathBuf::from("src/c.rs")]);
        assert_eq!(risks.len(), 1);
        // 12 callers across 4 modules, no tests, large blast radius → high risk
        assert!(risks[0].score > 0.5, "expected high risk, got {}", risks[0].score);
        assert_eq!(risks[0].factors.caller_count, 12);
        assert!(risks[0].factors.cross_module_count >= 4);
        assert!(!risks[0].factors.has_test_coverage);
    }

    #[test]
    fn change_risk_low_for_leaf() {
        let g = sample_graph();
        let risks = g.change_risk(&[PathBuf::from("src/a.rs")]);
        assert_eq!(risks.len(), 1);
        // a.rs: 0 callers, no blast radius → low risk
        assert!(risks[0].score < 0.4, "expected low risk, got {}", risks[0].score);
        assert_eq!(risks[0].factors.caller_count, 0);
        assert_eq!(risks[0].factors.blast_radius_size, 0);
    }

    #[test]
    fn change_risk_security_file() {
        let tags = vec![
            FileTag { rel_path: PathBuf::from("src/auth.rs"), line: 1, name: "Login".into(), kind: TagKind::Definition },
            FileTag { rel_path: PathBuf::from("src/main.rs"), line: 1, name: "Login".into(), kind: TagKind::Reference },
        ];
        let g = ReferenceGraph::build(&tags);
        let risks = g.change_risk(&[PathBuf::from("src/auth.rs")]);
        assert_eq!(risks.len(), 1);
        assert!(risks[0].factors.security_sensitive);
    }

    #[test]
    fn change_risk_with_test_coverage() {
        let tags = vec![
            FileTag { rel_path: PathBuf::from("src/lib.rs"), line: 1, name: "Func".into(), kind: TagKind::Definition },
            FileTag { rel_path: PathBuf::from("tests/test_lib.rs"), line: 1, name: "Func".into(), kind: TagKind::Reference },
        ];
        let g = ReferenceGraph::build(&tags);
        let risks = g.change_risk(&[PathBuf::from("src/lib.rs")]);
        assert_eq!(risks.len(), 1);
        assert!(risks[0].factors.has_test_coverage);
    }

    #[test]
    fn change_risk_unknown_file() {
        let g = sample_graph();
        let risks = g.change_risk(&[PathBuf::from("src/unknown.rs")]);
        assert_eq!(risks.len(), 1);
        assert_eq!(risks[0].score, 0.0);
    }
}
