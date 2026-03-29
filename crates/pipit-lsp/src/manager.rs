//! LSP server lifecycle manager.
//!
//! Discovers which LSP servers are needed, launches them, and provides
//! a unified interface for the agent to query semantic information.

use crate::{LspKind, DefinitionResult, ReferencesResult, TypeInfo};
use crate::client::LspClient;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Manages all active LSP server connections.
pub struct LspManager {
    clients: HashMap<LspKind, LspClient>,
    project_root: PathBuf,
}

impl LspManager {
    /// Auto-detect and start LSP servers for the project.
    pub async fn start_for_project(project_root: &Path) -> Self {
        let kinds = LspKind::detect_for_project(project_root);
        let mut clients = HashMap::new();

        for kind in kinds {
            match LspClient::start(kind, project_root).await {
                Ok(client) => {
                    tracing::info!(server = kind.binary(), "LSP server started");
                    clients.insert(kind, client);
                }
                Err(e) => {
                    tracing::debug!(server = kind.binary(), error = %e, "LSP server not available");
                }
            }
        }

        Self {
            clients,
            project_root: project_root.to_path_buf(),
        }
    }

    /// Get the LSP kind for a file extension.
    fn kind_for_file(&self, file: &Path) -> Option<LspKind> {
        let ext = file.extension()?.to_str()?;
        match ext {
            "rs" => Some(LspKind::RustAnalyzer),
            "py" => Some(LspKind::Pyright),
            "ts" | "tsx" | "js" | "jsx" => Some(LspKind::TypescriptLanguageServer),
            "go" => Some(LspKind::Gopls),
            _ => None,
        }
    }

    /// Go-to-definition — resolves what a symbol points to.
    pub async fn goto_definition(&self, file: &Path, line: u32, col: u32) -> Option<DefinitionResult> {
        let kind = self.kind_for_file(file)?;
        let client = self.clients.get(&kind)?;
        client.goto_definition(file, line, col).await.ok()
    }

    /// Find all references to a symbol.
    pub async fn find_references(&self, file: &Path, line: u32, col: u32) -> Option<ReferencesResult> {
        let kind = self.kind_for_file(file)?;
        let client = self.clients.get(&kind)?;
        client.find_references(file, line, col).await.ok()
    }

    /// Get type/hover info for a symbol.
    pub async fn hover(&self, file: &Path, line: u32, col: u32) -> Option<TypeInfo> {
        let kind = self.kind_for_file(file)?;
        let client = self.clients.get(&kind)?;
        client.hover(file, line, col).await.ok()?
    }

    /// List all active LSP servers.
    pub fn active_servers(&self) -> Vec<&str> {
        self.clients.keys().map(|k| k.binary()).collect()
    }

    /// Shut down all LSP servers.
    pub async fn shutdown_all(&self) {
        for client in self.clients.values() {
            client.shutdown().await;
        }
    }
}
