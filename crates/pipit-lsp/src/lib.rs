//! pipit-lsp: Language Server Protocol integration for IDE-grade code intelligence.
//!
//! Launches and manages LSP servers (rust-analyzer, pyright, typescript-language-server)
//! to provide semantic code intelligence beyond tree-sitter syntax analysis.
//!
//! Capabilities:
//! - Go-to-definition for symbol resolution
//! - Type information for smarter edits
//! - Diagnostics without full builds
//! - Cross-file rename refactoring
//! - Find all references

pub mod client;
pub mod manager;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Supported language server types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LspKind {
    RustAnalyzer,
    Pyright,
    TypescriptLanguageServer,
    Gopls,
}

impl LspKind {
    /// Binary name for this LSP server.
    pub fn binary(&self) -> &str {
        match self {
            Self::RustAnalyzer => "rust-analyzer",
            Self::Pyright => "pyright-langserver",
            Self::TypescriptLanguageServer => "typescript-language-server",
            Self::Gopls => "gopls",
        }
    }

    /// Detect which LSP servers are needed based on project files.
    pub fn detect_for_project(root: &Path) -> Vec<Self> {
        let mut servers = Vec::new();
        if root.join("Cargo.toml").exists() {
            servers.push(Self::RustAnalyzer);
        }
        if root.join("pyproject.toml").exists()
            || root.join("setup.py").exists()
            || root.join("requirements.txt").exists()
        {
            servers.push(Self::Pyright);
        }
        if root.join("tsconfig.json").exists() || root.join("package.json").exists() {
            servers.push(Self::TypescriptLanguageServer);
        }
        if root.join("go.mod").exists() {
            servers.push(Self::Gopls);
        }
        servers
    }
}

/// A location in source code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceLocation {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
}

/// Result of a go-to-definition request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionResult {
    pub locations: Vec<SourceLocation>,
}

/// Result of a find-references request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferencesResult {
    pub locations: Vec<SourceLocation>,
}

/// A diagnostic from the language server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub column: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub code: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Information,
    Hint,
}

/// Type information for a symbol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeInfo {
    pub symbol: String,
    pub type_string: String,
    pub documentation: Option<String>,
}
