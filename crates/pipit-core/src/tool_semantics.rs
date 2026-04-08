//! Semantic Tool Type System (Tool/Skill Task 1)
//!
//! Replaces shallow `is_mutating()`/`requires_approval()` booleans with typed
//! capability vectors, purity flags, and resource declarations. Permission
//! checks become subset tests; scheduling becomes resource-conflict detection.
//!
//! Every tool declares:
//! - Required capabilities (bitset from capability lattice)
//! - Resource footprint (paths, process, network, mcp, delegate)
//! - Purity classification (pure, idempotent, mutating, destructive)
//! - Commutativity with other tools

use crate::capability::CapabilitySet;
use crate::scheduler::{AccessMode, Resource, ResourceAccess, ResourceSignature};
use serde::{Deserialize, Serialize};

/// Semantic purity classification for tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Purity {
    /// No side effects; always safe to cache or parallelize.
    /// Examples: read_file, grep, glob, list_directory.
    Pure,
    /// Side-effect-free from the caller's perspective; may have internal state.
    /// Calling twice with the same args yields the same visible result.
    /// Examples: read with cache warm-up, search with index build.
    Idempotent,
    /// Has side effects but is undoable.
    /// Examples: edit_file, write_file (with file history).
    Mutating,
    /// Has side effects that are difficult or impossible to undo.
    /// Examples: bash (arbitrary shell), network POST, git push.
    Destructive,
}

/// The semantic type descriptor for a tool.
#[derive(Debug, Clone)]
pub struct ToolSemantics {
    /// Required capabilities from the permission lattice.
    pub required_capabilities: CapabilitySet,
    /// Purity classification.
    pub purity: Purity,
    /// Whether the tool is commutative with itself.
    /// If true, two calls with different args can be reordered safely.
    pub self_commutative: bool,
    /// Resource footprint factory: given args, produce a resource signature.
    /// None means "use default extraction from tool name + args".
    pub static_resources: Option<ResourceSignature>,
    /// Maximum expected execution time (seconds).
    pub expected_duration_secs: u32,
    /// Whether this tool's output should be stored in the blob store.
    pub large_output_likely: bool,
    /// Human-readable category for grouping/display.
    pub category: ToolCategory,
}

/// Tool category for grouping and display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCategory {
    /// File system operations.
    FileSystem,
    /// Search and discovery.
    Search,
    /// Code editing.
    Edit,
    /// Shell/process execution.
    Shell,
    /// Repository analysis.
    Analysis,
    /// External services (MCP).
    External,
    /// Agent delegation.
    Delegation,
    /// Verification.
    Verification,
}

impl ToolSemantics {
    /// Create semantics for a pure read-only tool.
    pub fn pure_read() -> Self {
        Self {
            required_capabilities: CapabilitySet::READ_ONLY,
            purity: Purity::Pure,
            self_commutative: true,
            static_resources: None,
            expected_duration_secs: 5,
            large_output_likely: false,
            category: ToolCategory::FileSystem,
        }
    }

    /// Create semantics for a mutating edit tool.
    pub fn mutating_edit() -> Self {
        Self {
            required_capabilities: CapabilitySet::EDIT,
            purity: Purity::Mutating,
            self_commutative: false,
            static_resources: None,
            expected_duration_secs: 5,
            large_output_likely: false,
            category: ToolCategory::Edit,
        }
    }

    /// Create semantics for a shell execution tool.
    pub fn shell_exec() -> Self {
        use crate::capability::Capability;
        Self {
            required_capabilities: CapabilitySet::EMPTY
                .grant(Capability::ProcessExec)
                .grant(Capability::ProcessExecMutating),
            purity: Purity::Destructive,
            self_commutative: false,
            static_resources: Some(ResourceSignature::process()),
            expected_duration_secs: 60,
            large_output_likely: true,
            category: ToolCategory::Shell,
        }
    }

    /// Create semantics for a search tool.
    pub fn search() -> Self {
        Self {
            required_capabilities: CapabilitySet::READ_ONLY,
            purity: Purity::Pure,
            self_commutative: true,
            static_resources: None,
            expected_duration_secs: 10,
            large_output_likely: true,
            category: ToolCategory::Search,
        }
    }

    /// Create semantics for an MCP tool with unknown purity.
    pub fn mcp_unknown(server_name: &str) -> Self {
        use crate::capability::Capability;
        Self {
            required_capabilities: CapabilitySet::EMPTY.grant(Capability::McpInvoke),
            purity: Purity::Destructive, // Conservative default
            self_commutative: false,
            static_resources: Some(ResourceSignature {
                accesses: vec![ResourceAccess {
                    resource: Resource::Mcp(server_name.to_string()),
                    mode: AccessMode::Write,
                }],
            }),
            expected_duration_secs: 30,
            large_output_likely: false,
            category: ToolCategory::External,
        }
    }

    /// Create semantics for an MCP tool known to be read-only.
    pub fn mcp_read_only(server_name: &str) -> Self {
        use crate::capability::Capability;
        Self {
            required_capabilities: CapabilitySet::EMPTY.grant(Capability::McpInvoke),
            purity: Purity::Pure,
            self_commutative: true,
            static_resources: Some(ResourceSignature {
                accesses: vec![ResourceAccess {
                    resource: Resource::Mcp(server_name.to_string()),
                    mode: AccessMode::Read,
                }],
            }),
            expected_duration_secs: 10,
            large_output_likely: false,
            category: ToolCategory::External,
        }
    }

    /// Create semantics for a subagent delegation tool.
    pub fn delegation() -> Self {
        use crate::capability::Capability;
        Self {
            required_capabilities: CapabilitySet::EMPTY.grant(Capability::Delegate),
            purity: Purity::Mutating,
            self_commutative: false,
            static_resources: Some(ResourceSignature {
                accesses: vec![ResourceAccess {
                    resource: Resource::Delegation,
                    mode: AccessMode::Write,
                }],
            }),
            expected_duration_secs: 300,
            large_output_likely: true,
            category: ToolCategory::Delegation,
        }
    }

    /// Whether this tool can run concurrently with another tool of the same type.
    pub fn can_parallelize_with_self(&self) -> bool {
        self.self_commutative && self.purity <= Purity::Idempotent
    }

    /// Whether approval should be required based on purity alone.
    pub fn needs_approval_by_purity(&self) -> bool {
        matches!(self.purity, Purity::Mutating | Purity::Destructive)
    }
}

/// Built-in semantic type mappings for known tools.
pub fn builtin_semantics(tool_name: &str) -> ToolSemantics {
    match tool_name {
        "read_file" => ToolSemantics::pure_read(),
        "list_directory" => ToolSemantics::pure_read(),
        "grep" => ToolSemantics {
            category: ToolCategory::Search,
            large_output_likely: true,
            ..ToolSemantics::search()
        },
        "glob" => ToolSemantics::search(),
        "edit_file" => ToolSemantics::mutating_edit(),
        "multi_edit_file" => ToolSemantics::mutating_edit(),
        "write_file" => ToolSemantics::mutating_edit(),
        "bash" => ToolSemantics::shell_exec(),
        "subagent" => ToolSemantics::delegation(),
        "structured_output" => ToolSemantics {
            required_capabilities: CapabilitySet::EMPTY,
            purity: Purity::Pure,
            self_commutative: true,
            static_resources: None,
            expected_duration_secs: 1,
            large_output_likely: false,
            category: ToolCategory::Analysis,
        },
        _ => ToolSemantics::mcp_unknown("unknown"),
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Canonical Semantic Descriptor — single source of truth for evidence
// ═══════════════════════════════════════════════════════════════════════

/// A canonical semantic descriptor for a tool call. Both `ActionClass` (risk)
/// and `EvidenceArtifact` (proof) are derived from this same structure.
///
/// This closed sum type replaces all string-dispatch in the governor and
/// evidence pipeline с total pattern matching over algebraic types.
#[derive(Debug, Clone)]
pub enum SemanticClass {
    /// Read from a path set (no side effects).
    Read { paths: Vec<String> },
    /// Search/query (no side effects, may produce large output).
    Search { query: Option<String> },
    /// Edit files at path set (undoable mutation).
    Edit { paths: Vec<String> },
    /// Execute a shell command (may have arbitrary side effects).
    Exec { command: String },
    /// Delegate to a subagent.
    Delegate { task: String },
    /// Invoke an external API/MCP server.
    External {
        server: Option<String>,
        tool: String,
    },
    /// Pure computation (no side effects, no I/O).
    Pure,
}

/// Derive the canonical semantic class from a tool name and its arguments.
/// This is the single derivation point — governor, evidence, scheduler,
/// and policy all consume this same descriptor.
pub fn classify_semantically(tool_name: &str, args: &serde_json::Value) -> SemanticClass {
    let semantics = builtin_semantics(tool_name);
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    match (semantics.category, semantics.purity) {
        (ToolCategory::FileSystem, Purity::Pure | Purity::Idempotent) => SemanticClass::Read {
            paths: path.into_iter().collect(),
        },
        (ToolCategory::Search, _) => SemanticClass::Search {
            query: args
                .get("pattern")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or(command),
        },
        (ToolCategory::Edit, _)
        | (ToolCategory::FileSystem, Purity::Mutating | Purity::Destructive) => {
            SemanticClass::Edit {
                paths: path.into_iter().collect(),
            }
        }
        (ToolCategory::Shell, _) => SemanticClass::Exec {
            command: command.unwrap_or_default(),
        },
        (ToolCategory::Delegation, _) => SemanticClass::Delegate {
            task: task.unwrap_or_default(),
        },
        (ToolCategory::External, _) => SemanticClass::External {
            server: None,
            tool: tool_name.to_string(),
        },
        (
            ToolCategory::Analysis | ToolCategory::Verification,
            Purity::Pure | Purity::Idempotent,
        ) => SemanticClass::Pure,
        _ => SemanticClass::External {
            server: None,
            tool: tool_name.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purity_ordering() {
        assert!(Purity::Pure < Purity::Idempotent);
        assert!(Purity::Idempotent < Purity::Mutating);
        assert!(Purity::Mutating < Purity::Destructive);
    }

    #[test]
    fn pure_tools_can_self_parallelize() {
        let read = ToolSemantics::pure_read();
        assert!(read.can_parallelize_with_self());

        let shell = ToolSemantics::shell_exec();
        assert!(!shell.can_parallelize_with_self());
    }

    #[test]
    fn builtin_semantics_coverage() {
        assert_eq!(builtin_semantics("read_file").purity, Purity::Pure);
        assert_eq!(builtin_semantics("edit_file").purity, Purity::Mutating);
        assert_eq!(builtin_semantics("bash").purity, Purity::Destructive);
        assert_eq!(
            builtin_semantics("subagent").category,
            ToolCategory::Delegation
        );
    }
}
