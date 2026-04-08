//! Pipit Memory — Persistent, human-editable agent memory (Task 5)
//!
//! Architecture:
//!   MEMORY.md (YAML frontmatter + markdown body)
//!     → Loaded at session start: O(n) where n = file size
//!     → Updated by agent via memory tool
//!     → Consolidated by auto-dream during idle periods
//!     → Synced across team via pipit-daemon (with secret scanning)
//!
//! Secret scanning: Aho-Corasick for fixed patterns + regex for structured
//! patterns (API keys, tokens, passwords). O(n·p) where p ~ 30 patterns.
//!
//! Memory truncation: If MEMORY.md exceeds L_max lines or B_max bytes,
//! truncate at last newline before B_max (O(n) single pass).

pub mod auto_dream;
pub mod memory_log;
pub mod secret_scanner;
pub mod team_sync;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ═══════════════════════════════════════════════════════════════════════════
//  Constants
// ═══════════════════════════════════════════════════════════════════════════

pub const MEMORY_FILE_NAME: &str = "MEMORY.md";
pub const MAX_MEMORY_LINES: usize = 200;
pub const MAX_MEMORY_BYTES: usize = 25_000;

// ═══════════════════════════════════════════════════════════════════════════
//  Memory Frontmatter
// ═══════════════════════════════════════════════════════════════════════════

/// YAML frontmatter metadata in MEMORY.md.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryFrontmatter {
    /// Schema version for migration.
    #[serde(default = "default_version")]
    pub version: u32,
    /// When this memory was last updated.
    pub last_updated: Option<String>,
    /// Who last updated (agent session ID or user).
    pub updated_by: Option<String>,
    /// Memory categories present.
    #[serde(default)]
    pub categories: Vec<String>,
    /// Team ID (for team memory sync).
    pub team_id: Option<String>,
    /// Whether this memory is shared (team) or personal.
    #[serde(default)]
    pub shared: bool,
}

fn default_version() -> u32 {
    1
}

// ═══════════════════════════════════════════════════════════════════════════
//  Memory Document
// ═══════════════════════════════════════════════════════════════════════════

/// A parsed MEMORY.md file.
#[derive(Debug, Clone)]
pub struct MemoryDocument {
    pub frontmatter: MemoryFrontmatter,
    pub body: String,
    pub source_path: PathBuf,
    pub was_truncated: bool,
}

impl MemoryDocument {
    /// Load a MEMORY.md file, parsing frontmatter and body.
    pub fn load(path: &Path) -> Result<Self, MemoryError> {
        let raw = std::fs::read_to_string(path).map_err(|e| MemoryError::Io(e.to_string()))?;

        let (frontmatter, body) = parse_frontmatter(&raw)?;
        let (body, was_truncated) = truncate_body(&body);

        Ok(Self {
            frontmatter,
            body,
            source_path: path.to_path_buf(),
            was_truncated,
        })
    }

    /// Create a new empty memory document.
    pub fn new_empty(path: &Path) -> Self {
        Self {
            frontmatter: MemoryFrontmatter {
                version: 1,
                last_updated: Some(chrono::Utc::now().to_rfc3339()),
                updated_by: Some("pipit".to_string()),
                categories: vec!["project".to_string(), "preferences".to_string()],
                team_id: None,
                shared: false,
            },
            body: String::new(),
            source_path: path.to_path_buf(),
            was_truncated: false,
        }
    }

    /// Save the memory document back to disk.
    pub fn save(&self) -> Result<(), MemoryError> {
        let mut content = String::new();

        // Write YAML frontmatter
        content.push_str("---\n");
        let yaml = serde_yaml_ng::to_string(&self.frontmatter)
            .map_err(|e| MemoryError::Serialization(e.to_string()))?;
        content.push_str(&yaml);
        content.push_str("---\n\n");

        // Write body
        content.push_str(&self.body);

        // Ensure parent directory exists
        if let Some(parent) = self.source_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MemoryError::Io(e.to_string()))?;
        }

        std::fs::write(&self.source_path, &content).map_err(|e| MemoryError::Io(e.to_string()))?;

        Ok(())
    }

    /// Add a memory entry to the body.
    pub fn add_entry(&mut self, category: &str, entry: &str) {
        // Find the category section or create one
        let section_header = format!("## {category}");

        if self.body.contains(&section_header) {
            // Append to existing section
            if let Some(pos) = self.body.find(&section_header) {
                // Find the end of this section (next ## or EOF)
                let after_header = pos + section_header.len();
                let next_section = self.body[after_header..]
                    .find("\n## ")
                    .map(|p| p + after_header)
                    .unwrap_or(self.body.len());

                self.body
                    .insert_str(next_section, &format!("\n- {entry}\n"));
            }
        } else {
            // Create new section
            self.body
                .push_str(&format!("\n{section_header}\n\n- {entry}\n"));
            if !self.frontmatter.categories.contains(&category.to_string()) {
                self.frontmatter.categories.push(category.to_string());
            }
        }

        self.frontmatter.last_updated = Some(chrono::Utc::now().to_rfc3339());
    }

    /// Get the full content as a prompt string (for system prompt injection).
    pub fn as_prompt(&self) -> String {
        if self.body.trim().is_empty() {
            return String::new();
        }

        let mut prompt = String::new();
        prompt.push_str("<memory>\n");
        prompt.push_str("The following is the user's persistent memory (MEMORY.md).\n");
        prompt.push_str("Respect these preferences and context in your responses.\n\n");
        prompt.push_str(&self.body);
        if self.was_truncated {
            prompt.push_str("\n\n[Memory truncated — file exceeds size limit]\n");
        }
        prompt.push_str("</memory>\n");
        prompt
    }

    /// Estimate token count (rough: 1 token ≈ 4 chars for English).
    pub fn estimated_tokens(&self) -> usize {
        self.body.len() / 4
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Frontmatter Parsing
// ═══════════════════════════════════════════════════════════════════════════

fn parse_frontmatter(raw: &str) -> Result<(MemoryFrontmatter, String), MemoryError> {
    let trimmed = raw.trim_start();

    if !trimmed.starts_with("---") {
        // No frontmatter — treat entire file as body
        return Ok((MemoryFrontmatter::default(), raw.to_string()));
    }

    // Find closing ---
    let after_open = &trimmed[3..];
    let close_pos = after_open
        .find("\n---")
        .ok_or_else(|| MemoryError::ParseError("Unclosed frontmatter".into()))?;

    let yaml_str = &after_open[..close_pos].trim();
    let body_start = 3 + close_pos + 4; // "---" + "\n---"
    let body = trimmed[body_start..].trim_start().to_string();

    let frontmatter: MemoryFrontmatter = serde_yaml_ng::from_str(yaml_str).unwrap_or_default();

    Ok((frontmatter, body))
}

// ═══════════════════════════════════════════════════════════════════════════
//  Body Truncation
// ═══════════════════════════════════════════════════════════════════════════

/// Truncate body to MAX_MEMORY_LINES and MAX_MEMORY_BYTES.
/// Returns (truncated_body, was_truncated).
fn truncate_body(body: &str) -> (String, bool) {
    let mut truncated = false;
    let mut result = body.to_string();

    // Line truncation first (natural boundary)
    let lines: Vec<&str> = result.lines().collect();
    if lines.len() > MAX_MEMORY_LINES {
        result = lines[..MAX_MEMORY_LINES].join("\n");
        truncated = true;
    }

    // Byte truncation at last newline before limit
    if result.len() > MAX_MEMORY_BYTES {
        let truncated_view = &result[..MAX_MEMORY_BYTES];
        if let Some(last_newline) = truncated_view.rfind('\n') {
            result = result[..last_newline].to_string();
        } else {
            result = truncated_view.to_string();
        }
        truncated = true;
    }

    (result, truncated)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Memory Manager
// ═══════════════════════════════════════════════════════════════════════════

/// Manages memory loading, saving, and resolution across multiple sources.
///
/// Memory resolution order:
///   1. Project memory: .pipit/MEMORY.md
///   2. Global memory: ~/.config/pipit/MEMORY.md
///   3. Team memory: .pipit/team/MEMORY.md (if team sync enabled)
pub struct MemoryManager {
    project_memory: Option<MemoryDocument>,
    global_memory: Option<MemoryDocument>,
    team_memory: Option<MemoryDocument>,
    project_root: PathBuf,
    /// Log-structured write-ahead log for programmatic memory writes.
    /// Candidates flow: append → secret scan → dedup → commit → project to MEMORY.md.
    memory_log: memory_log::MemoryLog,
}

impl MemoryManager {
    /// Initialize memory manager for a project.
    pub fn new(project_root: &Path) -> Self {
        let project_mem_path = project_root.join(".pipit").join(MEMORY_FILE_NAME);
        let global_mem_path = dirs_path().join(MEMORY_FILE_NAME);
        let team_mem_path = project_root
            .join(".pipit")
            .join("team")
            .join(MEMORY_FILE_NAME);

        let project_memory = MemoryDocument::load(&project_mem_path).ok();
        let global_memory = MemoryDocument::load(&global_mem_path).ok();
        let team_memory = MemoryDocument::load(&team_mem_path).ok();

        if let Some(ref pm) = project_memory {
            tracing::info!(
                lines = pm.body.lines().count(),
                bytes = pm.body.len(),
                truncated = pm.was_truncated,
                "Loaded project memory"
            );
        }

        // Initialize the log-structured WAL and index existing entries for dedup
        let mut memory_log = memory_log::MemoryLog::open(project_root);
        if let Some(ref pm) = project_memory {
            memory_log.index_existing_memory(&pm.body);
        }
        if let Some(ref gm) = global_memory {
            memory_log.index_existing_memory(&gm.body);
        }

        Self {
            project_memory,
            global_memory,
            team_memory,
            project_root: project_root.to_path_buf(),
            memory_log,
        }
    }

    /// Build the combined memory prompt for system prompt injection.
    pub fn build_prompt(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ref mem) = self.global_memory {
            let prompt = mem.as_prompt();
            if !prompt.is_empty() {
                parts.push(format!("<!-- Global preferences -->\n{prompt}"));
            }
        }

        if let Some(ref mem) = self.project_memory {
            let prompt = mem.as_prompt();
            if !prompt.is_empty() {
                parts.push(format!("<!-- Project memory -->\n{prompt}"));
            }
        }

        if let Some(ref mem) = self.team_memory {
            let prompt = mem.as_prompt();
            if !prompt.is_empty() {
                parts.push(format!("<!-- Team memory -->\n{prompt}"));
            }
        }

        parts.join("\n\n")
    }

    /// Get or create the project memory document.
    pub fn project_memory_mut(&mut self) -> &mut MemoryDocument {
        if self.project_memory.is_none() {
            let path = self.project_root.join(".pipit").join(MEMORY_FILE_NAME);
            self.project_memory = Some(MemoryDocument::new_empty(&path));
        }
        self.project_memory.as_mut().unwrap()
    }

    /// Save all modified memory documents.
    pub fn save_all(&self) -> Result<(), MemoryError> {
        if let Some(ref mem) = self.project_memory {
            mem.save()?;
        }
        if let Some(ref mem) = self.global_memory {
            mem.save()?;
        }
        if let Some(ref mem) = self.team_memory {
            mem.save()?;
        }
        Ok(())
    }

    /// Total estimated tokens across all memory sources.
    pub fn total_tokens(&self) -> usize {
        let mut total = 0;
        if let Some(ref m) = self.project_memory {
            total += m.estimated_tokens();
        }
        if let Some(ref m) = self.global_memory {
            total += m.estimated_tokens();
        }
        if let Some(ref m) = self.team_memory {
            total += m.estimated_tokens();
        }
        total
    }

    // ── Log-Structured Write Path ──
    // All programmatic writes go through the memory log pipeline:
    //   append_candidate → secret_scan → dedup → commit → project to MEMORY.md.
    // Direct `add_entry` on MemoryDocument is still available for manual edits.

    /// Append a memory candidate through the log-structured pipeline.
    /// The candidate is persisted to the WAL immediately (crash-safe).
    /// Call `flush_pending()` to process and project to MEMORY.md.
    pub fn append_memory(
        &mut self,
        text: &str,
        category: &str,
        source: &str,
        salience: f64,
    ) -> Result<u64, MemoryError> {
        self.memory_log
            .append_candidate(text, category, source, salience)
    }

    /// Process all pending candidates and project committed ones to MEMORY.md.
    /// Pipeline: secret_scan → dedup → commit → write MEMORY.md → compact log.
    /// Returns (committed_count, rejected_count).
    pub fn flush_pending(&mut self) -> Result<(usize, usize), MemoryError> {
        let (committed, rejected) = self.memory_log.process_pending()?;

        if committed > 0 {
            // Get committed entries first (borrows memory_log immutably)
            let entries = self.memory_log.committed_entries()?;

            // Ensure project memory exists
            if self.project_memory.is_none() {
                let path = self.project_root.join(".pipit").join(MEMORY_FILE_NAME);
                self.project_memory = Some(MemoryDocument::new_empty(&path));
            }

            // Project committed entries to MEMORY.md
            let mem = self.project_memory.as_mut().unwrap();
            for entry in &entries {
                let (sanitized, findings) = crate::secret_scanner::sanitize(&entry.text);
                if !findings.is_empty() {
                    tracing::warn!(
                        "Secret found during projection — redacting {} findings",
                        findings.len()
                    );
                }
                mem.add_entry(&entry.category, &sanitized);
            }
            mem.frontmatter.last_updated = Some(chrono::Utc::now().to_rfc3339());
            mem.save()?;

            // Compact the log
            self.memory_log.compact()?;

            tracing::info!(
                committed,
                rejected,
                projected = entries.len(),
                "Memory log: processed candidates"
            );
        }

        Ok((committed, rejected))
    }

    /// Get the memory log for diagnostics.
    pub fn memory_log(&self) -> &memory_log::MemoryLog {
        &self.memory_log
    }

    /// Get mutable access to the memory log.
    pub fn memory_log_mut(&mut self) -> &mut memory_log::MemoryLog {
        &mut self.memory_log
    }
}

fn dirs_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("~/.config"))
        .join("pipit")
}

// ═══════════════════════════════════════════════════════════════════════════
//  Errors
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("Parse error: {0}")]
    ParseError(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Secret detected: {0}")]
    SecretDetected(String),
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parse_frontmatter_with_yaml() {
        let raw = r#"---
version: 1
last_updated: "2025-01-01T00:00:00Z"
categories:
  - project
  - coding
---

## Project

- This is a Rust project using Tokio
- Uses pipit for AI-assisted coding

## Preferences

- Prefer explicit types over inference
"#;
        let (fm, body) = parse_frontmatter(raw).unwrap();
        assert_eq!(fm.version, 1);
        assert_eq!(fm.categories, vec!["project", "coding"]);
        assert!(body.contains("## Project"));
        assert!(body.contains("Prefer explicit types"));
    }

    #[test]
    fn parse_no_frontmatter() {
        let raw = "## Notes\n- Just some notes\n";
        let (fm, body) = parse_frontmatter(raw).unwrap();
        assert_eq!(fm.version, 0); // default
        assert!(body.contains("## Notes"));
    }

    #[test]
    fn truncation_by_lines() {
        let body = (0..300)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (truncated, was_truncated) = truncate_body(&body);
        assert!(was_truncated);
        assert!(truncated.lines().count() <= MAX_MEMORY_LINES);
    }

    #[test]
    fn truncation_by_bytes() {
        let body = "x".repeat(30_000);
        let (truncated, was_truncated) = truncate_body(&body);
        assert!(was_truncated);
        assert!(truncated.len() <= MAX_MEMORY_BYTES);
    }

    #[test]
    fn add_entry_to_memory() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        let mut doc = MemoryDocument::new_empty(&path);

        doc.add_entry("project", "Uses Rust 1.75+");
        doc.add_entry("project", "Async-first architecture");
        doc.add_entry("preferences", "No unwrap() in production code");

        assert!(doc.body.contains("## project"));
        assert!(doc.body.contains("Uses Rust 1.75+"));
        assert!(doc.body.contains("## preferences"));

        // Save and reload
        doc.save().unwrap();
        let loaded = MemoryDocument::load(&path).unwrap();
        assert!(loaded.body.contains("Async-first architecture"));
    }

    #[test]
    fn prompt_generation() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("MEMORY.md");
        let mut doc = MemoryDocument::new_empty(&path);
        doc.add_entry("coding", "Always use error handling");

        let prompt = doc.as_prompt();
        assert!(prompt.contains("<memory>"));
        assert!(prompt.contains("Always use error handling"));
        assert!(prompt.contains("</memory>"));
    }
}
