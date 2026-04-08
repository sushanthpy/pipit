//! Skill Pipeline — DAG-based skill composition.
//!
//! Transforms the linear "one prompt → one agent loop → one outcome"
//! model into a directed acyclic graph of skill invocations where
//! Skill A's output wires into Skill B's input.
//!
//! Design:
//! - PipelineNode: a skill invocation with typed input bindings
//! - PipelineEdge: data flow from one node's output to another's input
//! - SkillPipeline: the DAG + topological executor
//!
//! Execution model: topological sort → sequential execution per level,
//! with parallelism within each topological level (independent nodes).
//!
//! ```text
//! [code-review] ──findings──→ [security-scan] ──report──→ [summarize]
//!       ↓                                                       ↑
//!    [lint] ─────────────────────warnings────────────────────────┘
//! ```

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// Unique identifier for a node in the pipeline.
pub type NodeId = String;

/// A node in the skill pipeline DAG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineNode {
    /// Unique node ID within this pipeline.
    pub id: NodeId,
    /// Name of the skill to invoke.
    pub skill_name: String,
    /// Static input values (not from edges).
    pub static_inputs: HashMap<String, serde_json::Value>,
    /// Input bindings from upstream edges: local_param → (source_node, source_param).
    #[serde(default)]
    pub input_bindings: HashMap<String, (NodeId, String)>,
}

/// The pipeline DAG definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillPipeline {
    /// Human-readable name.
    pub name: String,
    /// All nodes in the pipeline.
    pub nodes: Vec<PipelineNode>,
}

/// Output captured from a single node execution.
#[derive(Debug, Clone)]
pub struct NodeOutput {
    pub node_id: NodeId,
    pub skill_name: String,
    pub outputs: HashMap<String, serde_json::Value>,
    pub success: bool,
    pub elapsed_ms: u64,
    pub error: Option<String>,
}

/// Execution plan: topologically sorted levels of node IDs.
/// Nodes within the same level have no dependencies on each other
/// and can execute in parallel.
#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    /// Each inner Vec is a topological level (parallelizable).
    pub levels: Vec<Vec<NodeId>>,
}

/// Error in pipeline construction or execution.
#[derive(Debug, Clone)]
pub enum PipelineError {
    CycleDetected,
    MissingNode(String),
    MissingBinding {
        node: String,
        param: String,
        source_node: String,
    },
    DuplicateNodeId(String),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PipelineError::CycleDetected => write!(f, "Pipeline contains a cycle"),
            PipelineError::MissingNode(id) => write!(f, "Reference to missing node: {}", id),
            PipelineError::MissingBinding {
                node,
                param,
                source_node,
            } => {
                write!(
                    f,
                    "Node '{}' binds param '{}' from missing node '{}'",
                    node, param, source_node
                )
            }
            PipelineError::DuplicateNodeId(id) => write!(f, "Duplicate node ID: {}", id),
        }
    }
}

impl std::error::Error for PipelineError {}

impl SkillPipeline {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nodes: Vec::new(),
        }
    }

    /// Add a node to the pipeline.
    pub fn add_node(&mut self, node: PipelineNode) {
        self.nodes.push(node);
    }

    /// Validate the pipeline DAG: no cycles, all references valid.
    pub fn validate(&self) -> Result<(), PipelineError> {
        let node_ids: HashSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();

        // Check for duplicate IDs
        if node_ids.len() != self.nodes.len() {
            let mut seen = HashSet::new();
            for n in &self.nodes {
                if !seen.insert(&n.id) {
                    return Err(PipelineError::DuplicateNodeId(n.id.clone()));
                }
            }
        }

        // Check all binding references point to existing nodes
        for node in &self.nodes {
            for (param, (source_node, _)) in &node.input_bindings {
                if !node_ids.contains(source_node.as_str()) {
                    return Err(PipelineError::MissingBinding {
                        node: node.id.clone(),
                        param: param.clone(),
                        source_node: source_node.clone(),
                    });
                }
            }
        }

        // Cycle detection via topological sort (Kahn's algorithm)
        self.topological_sort()?;

        Ok(())
    }

    /// Compute the execution plan: topological levels for parallel execution.
    pub fn execution_plan(&self) -> Result<ExecutionPlan, PipelineError> {
        let sorted = self.topological_sort()?;
        Ok(ExecutionPlan { levels: sorted })
    }

    /// Kahn's algorithm producing levels (not a flat ordering).
    /// Each level contains nodes whose dependencies are all in prior levels.
    /// Time: O(V + E), Space: O(V).
    fn topological_sort(&self) -> Result<Vec<Vec<NodeId>>, PipelineError> {
        let n = self.nodes.len();
        let node_map: HashMap<&str, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();

        // Build adjacency + in-degree
        let mut in_degree = vec![0u32; n];
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (i, node) in self.nodes.iter().enumerate() {
            for (_, (source_id, _)) in &node.input_bindings {
                if let Some(&src_idx) = node_map.get(source_id.as_str()) {
                    adj[src_idx].push(i);
                    in_degree[i] += 1;
                }
            }
        }

        let mut queue: VecDeque<usize> = VecDeque::new();
        for (i, &deg) in in_degree.iter().enumerate() {
            if deg == 0 {
                queue.push_back(i);
            }
        }

        let mut levels = Vec::new();
        let mut processed = 0;

        while !queue.is_empty() {
            let level_size = queue.len();
            let mut level = Vec::with_capacity(level_size);

            for _ in 0..level_size {
                let idx = queue.pop_front().unwrap();
                level.push(self.nodes[idx].id.clone());
                processed += 1;

                for &neighbor in &adj[idx] {
                    in_degree[neighbor] -= 1;
                    if in_degree[neighbor] == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }

            levels.push(level);
        }

        if processed != n {
            return Err(PipelineError::CycleDetected);
        }

        Ok(levels)
    }

    /// Resolve inputs for a node given completed upstream outputs.
    pub fn resolve_inputs(
        &self,
        node: &PipelineNode,
        completed: &HashMap<NodeId, NodeOutput>,
    ) -> HashMap<String, serde_json::Value> {
        let mut inputs = node.static_inputs.clone();

        for (param, (source_node, source_param)) in &node.input_bindings {
            if let Some(output) = completed.get(source_node) {
                if let Some(value) = output.outputs.get(source_param) {
                    inputs.insert(param.clone(), value.clone());
                }
            }
        }

        inputs
    }

    /// Get node by ID.
    pub fn get_node(&self, id: &str) -> Option<&PipelineNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Number of nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Number of edges (input bindings).
    pub fn edge_count(&self) -> usize {
        self.nodes.iter().map(|n| n.input_bindings.len()).sum()
    }
}

/// Builder for constructing pipelines fluently.
pub struct PipelineBuilder {
    pipeline: SkillPipeline,
}

impl PipelineBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            pipeline: SkillPipeline::new(name),
        }
    }

    /// Add a skill node with static inputs only.
    pub fn add(
        mut self,
        id: impl Into<String>,
        skill: impl Into<String>,
        inputs: HashMap<String, serde_json::Value>,
    ) -> Self {
        self.pipeline.add_node(PipelineNode {
            id: id.into(),
            skill_name: skill.into(),
            static_inputs: inputs,
            input_bindings: HashMap::new(),
        });
        self
    }

    /// Add a skill node that reads from upstream.
    pub fn add_wired(
        mut self,
        id: impl Into<String>,
        skill: impl Into<String>,
        bindings: Vec<(&str, &str, &str)>, // (local_param, source_node, source_param)
    ) -> Self {
        let input_bindings: HashMap<String, (NodeId, String)> = bindings
            .into_iter()
            .map(|(local, src_node, src_param)| {
                (
                    local.to_string(),
                    (src_node.to_string(), src_param.to_string()),
                )
            })
            .collect();

        self.pipeline.add_node(PipelineNode {
            id: id.into(),
            skill_name: skill.into(),
            static_inputs: HashMap::new(),
            input_bindings,
        });
        self
    }

    /// Build and validate the pipeline.
    pub fn build(self) -> Result<SkillPipeline, PipelineError> {
        self.pipeline.validate()?;
        Ok(self.pipeline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_linear_pipeline() {
        let pipeline = PipelineBuilder::new("review-flow")
            .add("review", "code-review", HashMap::new())
            .add_wired(
                "scan",
                "security-scan",
                vec![("findings", "review", "findings")],
            )
            .add_wired("summary", "summarize", vec![("report", "scan", "report")])
            .build()
            .unwrap();

        let plan = pipeline.execution_plan().unwrap();
        assert_eq!(plan.levels.len(), 3);
        assert_eq!(plan.levels[0], vec!["review"]);
        assert_eq!(plan.levels[1], vec!["scan"]);
        assert_eq!(plan.levels[2], vec!["summary"]);
    }

    #[test]
    fn test_diamond_pipeline() {
        // review → security-scan ↘
        //                          → summary
        // review → lint          ↗
        let pipeline = PipelineBuilder::new("diamond")
            .add("review", "code-review", HashMap::new())
            .add_wired("scan", "security-scan", vec![("input", "review", "diff")])
            .add_wired("lint", "lint", vec![("input", "review", "diff")])
            .add_wired(
                "summary",
                "summarize",
                vec![
                    ("security", "scan", "report"),
                    ("warnings", "lint", "warnings"),
                ],
            )
            .build()
            .unwrap();

        let plan = pipeline.execution_plan().unwrap();
        assert_eq!(plan.levels.len(), 3); // review → {scan, lint} → summary
        assert_eq!(plan.levels[0].len(), 1); // review
        assert_eq!(plan.levels[1].len(), 2); // scan + lint (parallel)
        assert_eq!(plan.levels[2].len(), 1); // summary
    }

    #[test]
    fn test_cycle_detection() {
        let mut pipeline = SkillPipeline::new("cyclic");
        pipeline.add_node(PipelineNode {
            id: "a".into(),
            skill_name: "skill-a".into(),
            static_inputs: HashMap::new(),
            input_bindings: HashMap::from([("x".into(), ("b".into(), "y".into()))]),
        });
        pipeline.add_node(PipelineNode {
            id: "b".into(),
            skill_name: "skill-b".into(),
            static_inputs: HashMap::new(),
            input_bindings: HashMap::from([("y".into(), ("a".into(), "x".into()))]),
        });

        assert!(matches!(
            pipeline.validate(),
            Err(PipelineError::CycleDetected)
        ));
    }

    #[test]
    fn test_input_resolution() {
        let pipeline = PipelineBuilder::new("test")
            .add("src", "source", HashMap::new())
            .add_wired("dst", "dest", vec![("data", "src", "output")])
            .build()
            .unwrap();

        let mut completed = HashMap::new();
        completed.insert(
            "src".to_string(),
            NodeOutput {
                node_id: "src".into(),
                skill_name: "source".into(),
                outputs: HashMap::from([("output".into(), serde_json::json!("hello"))]),
                success: true,
                elapsed_ms: 100,
                error: None,
            },
        );

        let node = pipeline.get_node("dst").unwrap();
        let inputs = pipeline.resolve_inputs(node, &completed);
        assert_eq!(inputs["data"], serde_json::json!("hello"));
    }
}
