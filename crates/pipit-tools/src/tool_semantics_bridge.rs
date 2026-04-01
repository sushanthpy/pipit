//! Bridge from pipit-core's ToolSemantics to pipit-tools' runtime decisions.
//!
//! This module provides the single source of truth for tool authorization.
//! The per-tool `is_mutating()` / `requires_approval()` booleans are now
//! derived from the semantic type system rather than declared ad-hoc.

use pipit_config::ApprovalMode;

/// Purity level matching pipit-core's Purity enum.
/// Duplicated here to avoid circular dep (pipit-tools cannot depend on pipit-core).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Purity {
    Pure,
    Idempotent,
    Mutating,
    Destructive,
}

/// Minimal semantic descriptor that tools provide.
/// This is the pipit-tools side of the semantic type bridge.
#[derive(Debug, Clone)]
pub struct ToolSemanticsDescriptor {
    pub purity: Purity,
    /// If true, this tool is known to only read data.
    pub read_only: bool,
}

impl ToolSemanticsDescriptor {
    /// Derive `is_mutating` from purity.
    pub fn is_mutating(&self) -> bool {
        matches!(self.purity, Purity::Mutating | Purity::Destructive)
    }

    /// Derive `requires_approval` from purity and mode.
    /// This replaces per-tool ad-hoc approval logic with a uniform policy:
    /// - FullAuto: never ask
    /// - AutoEdit: ask for Destructive (shell, network, etc.)
    /// - CommandReview: ask for Mutating + Destructive
    /// - Suggest: always ask for anything non-Pure
    pub fn requires_approval(&self, mode: ApprovalMode) -> bool {
        match mode {
            ApprovalMode::FullAuto => false,
            ApprovalMode::AutoEdit => matches!(self.purity, Purity::Destructive),
            ApprovalMode::CommandReview => {
                matches!(self.purity, Purity::Mutating | Purity::Destructive)
            }
            ApprovalMode::Suggest => {
                matches!(self.purity, Purity::Mutating | Purity::Destructive)
            }
        }
    }
}

/// Built-in semantics lookup (mirrors pipit-core's builtin_semantics).
pub fn builtin_descriptor(tool_name: &str) -> ToolSemanticsDescriptor {
    match tool_name {
        "read_file" | "list_directory" | "grep" | "glob" => ToolSemanticsDescriptor {
            purity: Purity::Pure,
            read_only: true,
        },
        "edit_file" | "multi_edit_file" | "write_file" => ToolSemanticsDescriptor {
            purity: Purity::Mutating,
            read_only: false,
        },
        "bash" => ToolSemanticsDescriptor {
            purity: Purity::Destructive,
            read_only: false,
        },
        "subagent" => ToolSemanticsDescriptor {
            purity: Purity::Mutating,
            read_only: false,
        },
        "structured_output" => ToolSemanticsDescriptor {
            purity: Purity::Pure,
            read_only: true,
        },
        "mcp_search" => ToolSemanticsDescriptor {
            purity: Purity::Destructive, // can invoke arbitrary MCP tools
            read_only: false,
        },
        // MCP tools default to Destructive (conservative)
        _ => ToolSemanticsDescriptor {
            purity: Purity::Destructive,
            read_only: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_tool_never_needs_approval() {
        let desc = builtin_descriptor("read_file");
        assert!(!desc.requires_approval(ApprovalMode::Suggest));
        assert!(!desc.requires_approval(ApprovalMode::FullAuto));
        assert!(!desc.is_mutating());
    }

    #[test]
    fn destructive_tool_needs_approval_except_full_auto() {
        let desc = builtin_descriptor("bash");
        assert!(desc.requires_approval(ApprovalMode::Suggest));
        assert!(desc.requires_approval(ApprovalMode::AutoEdit));
        assert!(!desc.requires_approval(ApprovalMode::FullAuto));
        assert!(desc.is_mutating());
    }

    #[test]
    fn mutating_tool_approval_depends_on_mode() {
        let desc = builtin_descriptor("write_file");
        assert!(desc.requires_approval(ApprovalMode::CommandReview));
        assert!(!desc.requires_approval(ApprovalMode::AutoEdit));
        assert!(!desc.requires_approval(ApprovalMode::FullAuto));
    }
}
