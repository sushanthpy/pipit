//! Log-Structured Memory — Append-Only Write-Ahead Log for Memory Candidates
//!
//! Design: MEMORY.md is a human-editable projection. The MemoryLog is the
//! append-only source of truth for programmatic writes. Pipeline:
//!
//!   append_candidate → secret_scan → dedup_check → commit → project_to_md
//!
//! Complexities:
//!   - Append candidate: O(1)
//!   - Secret scan: O(n) in candidate text size
//!   - Dedup (Jaccard word-set overlap): O(m) per check against committed set
//!   - Projection to MEMORY.md: O(k) where k = committed entries
//!   - Compaction: O(k) — rewrite log keeping only committed entries
//!
//! The log file is `.pipit/memory-log.jsonl` — one JSON object per line.

use crate::secret_scanner;
use crate::MemoryError;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Status of a memory candidate in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateStatus {
    /// Appended but not yet processed.
    Pending,
    /// Secret detected — rejected.
    RejectedSecret,
    /// Duplicate of existing entry — rejected.
    RejectedDuplicate,
    /// Committed to memory (projected to MEMORY.md).
    Committed,
}

/// A memory candidate entry in the append-only log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryCandidate {
    /// Unique ID for this candidate.
    pub id: u64,
    /// When this candidate was appended.
    pub timestamp: String,
    /// The memory text.
    pub text: String,
    /// Category for the entry.
    pub category: String,
    /// Source (session ID, auto-dream, user).
    pub source: String,
    /// Current status.
    pub status: CandidateStatus,
    /// If rejected, why.
    pub rejection_reason: Option<String>,
    /// Salience score from auto-dream (0.0-1.0).
    pub salience: f64,
}

/// Fingerprint of a committed memory entry for dedup comparison.
/// Uses word-set for Jaccard similarity (O(m) per comparison).
#[derive(Debug, Clone)]
struct EntryFingerprint {
    words: HashSet<String>,
}

impl EntryFingerprint {
    fn from_text(text: &str) -> Self {
        let words: HashSet<String> = text
            .split_whitespace()
            .map(|w| w.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_string())
            .filter(|w| w.len() > 2)
            .collect();
        Self { words }
    }

    /// Jaccard similarity: |A ∩ B| / |A ∪ B|.
    /// Returns 0.0-1.0 (1.0 = identical word sets).
    fn similarity(&self, other: &EntryFingerprint) -> f64 {
        if self.words.is_empty() && other.words.is_empty() {
            return 1.0;
        }
        let intersection = self.words.intersection(&other.words).count();
        let union = self.words.union(&other.words).count();
        if union == 0 {
            return 0.0;
        }
        intersection as f64 / union as f64
    }
}

/// The memory log — append-only with pipeline processing.
pub struct MemoryLog {
    /// Path to the JSONL log file.
    log_path: PathBuf,
    /// Next candidate ID.
    next_id: u64,
    /// In-memory index of committed entry fingerprints (for dedup).
    committed_fingerprints: Vec<EntryFingerprint>,
    /// Jaccard threshold above which a candidate is considered duplicate.
    dedup_threshold: f64,
    /// Maximum committed entries to keep in the log after compaction.
    max_log_entries: usize,
}

impl MemoryLog {
    /// Open or create a memory log.
    pub fn open(project_root: &Path) -> Self {
        let log_path = project_root.join(".pipit").join("memory-log.jsonl");

        // Load existing committed entries for dedup index
        let (next_id, fingerprints) = if log_path.exists() {
            Self::load_index(&log_path)
        } else {
            (1, Vec::new())
        };

        Self {
            log_path,
            next_id,
            committed_fingerprints: fingerprints,
            dedup_threshold: 0.6,
            max_log_entries: 500,
        }
    }

    /// Also index existing MEMORY.md body entries for dedup.
    pub fn index_existing_memory(&mut self, memory_body: &str) {
        for line in memory_body.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("- ") {
                let entry_text = &trimmed[2..];
                if entry_text.len() > 10 {
                    self.committed_fingerprints.push(EntryFingerprint::from_text(entry_text));
                }
            }
        }
    }

    /// Append a candidate to the log. O(1) write.
    /// Returns the candidate ID.
    pub fn append_candidate(
        &mut self,
        text: &str,
        category: &str,
        source: &str,
        salience: f64,
    ) -> Result<u64, MemoryError> {
        let id = self.next_id;
        self.next_id += 1;

        let candidate = MemoryCandidate {
            id,
            timestamp: chrono::Utc::now().to_rfc3339(),
            text: text.to_string(),
            category: category.to_string(),
            source: source.to_string(),
            status: CandidateStatus::Pending,
            rejection_reason: None,
            salience,
        };

        self.append_to_file(&candidate)?;
        Ok(id)
    }

    /// Process all pending candidates through the pipeline.
    /// Returns (committed_count, rejected_count).
    pub fn process_pending(&mut self) -> Result<(usize, usize), MemoryError> {
        let entries = self.read_all()?;
        let mut committed = 0;
        let mut rejected = 0;
        let mut updates: Vec<MemoryCandidate> = Vec::new();

        for mut candidate in entries {
            if candidate.status != CandidateStatus::Pending {
                updates.push(candidate);
                continue;
            }

            // Stage 1: Secret scan
            if secret_scanner::contains_secrets(&candidate.text) {
                candidate.status = CandidateStatus::RejectedSecret;
                candidate.rejection_reason = Some("Secret detected in candidate text".into());
                rejected += 1;
                updates.push(candidate);
                continue;
            }

            // Stage 2: Dedup check (Jaccard similarity against committed entries)
            let candidate_fp = EntryFingerprint::from_text(&candidate.text);
            let is_dup = self.committed_fingerprints.iter().any(|fp| {
                candidate_fp.similarity(fp) > self.dedup_threshold
            });

            if is_dup {
                candidate.status = CandidateStatus::RejectedDuplicate;
                candidate.rejection_reason = Some(format!(
                    "Duplicate (Jaccard > {:.0}% overlap with existing entry)",
                    self.dedup_threshold * 100.0
                ));
                rejected += 1;
                updates.push(candidate);
                continue;
            }

            // Stage 3: Commit — add fingerprint to dedup index
            self.committed_fingerprints.push(candidate_fp);
            candidate.status = CandidateStatus::Committed;
            committed += 1;
            updates.push(candidate);
        }

        // Rewrite the log with updated statuses
        self.rewrite_log(&updates)?;

        Ok((committed, rejected))
    }

    /// Project committed entries to a MemoryDocument.
    /// Only returns entries that haven't been projected yet.
    pub fn committed_entries(&self) -> Result<Vec<MemoryCandidate>, MemoryError> {
        let entries = self.read_all()?;
        Ok(entries
            .into_iter()
            .filter(|e| e.status == CandidateStatus::Committed)
            .collect())
    }

    /// Project committed log entries into the memory document,
    /// then compact the log to remove processed entries.
    pub fn project_and_compact(
        &mut self,
        memory: &mut crate::MemoryDocument,
    ) -> Result<usize, MemoryError> {
        let committed = self.committed_entries()?;
        let count = committed.len();

        for entry in &committed {
            // Secret scan the text one final time before writing to MEMORY.md
            let (sanitized, findings) = secret_scanner::sanitize(&entry.text);
            if !findings.is_empty() {
                tracing::warn!(
                    "Secret found during projection — redacting {} findings",
                    findings.len()
                );
            }
            memory.add_entry(&entry.category, &sanitized);
        }

        if count > 0 {
            memory.frontmatter.last_updated = Some(chrono::Utc::now().to_rfc3339());
            memory.save()?;
        }

        // Compact: keep only the last N entries for audit trail
        self.compact()?;

        Ok(count)
    }

    /// Compact the log — remove old processed entries, keep last N.
    pub fn compact(&mut self) -> Result<(), MemoryError> {
        let entries = self.read_all()?;
        if entries.len() <= self.max_log_entries {
            return Ok(());
        }

        // Keep the last max_log_entries entries
        let keep = &entries[entries.len() - self.max_log_entries..];
        self.rewrite_log(keep)?;

        tracing::info!(
            removed = entries.len() - keep.len(),
            kept = keep.len(),
            "Memory log compacted"
        );

        Ok(())
    }

    /// Get log statistics for diagnostics.
    pub fn stats(&self) -> MemoryLogStats {
        let entries = self.read_all().unwrap_or_default();
        let pending = entries.iter().filter(|e| e.status == CandidateStatus::Pending).count();
        let committed = entries.iter().filter(|e| e.status == CandidateStatus::Committed).count();
        let rejected_secret = entries.iter().filter(|e| e.status == CandidateStatus::RejectedSecret).count();
        let rejected_dup = entries.iter().filter(|e| e.status == CandidateStatus::RejectedDuplicate).count();

        MemoryLogStats {
            total_entries: entries.len(),
            pending,
            committed,
            rejected_secret,
            rejected_duplicate: rejected_dup,
            fingerprint_index_size: self.committed_fingerprints.len(),
        }
    }

    // ── Internal helpers ──

    fn append_to_file(&self, candidate: &MemoryCandidate) -> Result<(), MemoryError> {
        if let Some(parent) = self.log_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryError::Io(e.to_string()))?;
        }

        let json = serde_json::to_string(candidate)
            .map_err(|e| MemoryError::Serialization(e.to_string()))?;

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
            .map_err(|e| MemoryError::Io(e.to_string()))?;

        writeln!(file, "{}", json)
            .map_err(|e| MemoryError::Io(e.to_string()))?;

        Ok(())
    }

    fn read_all(&self) -> Result<Vec<MemoryCandidate>, MemoryError> {
        if !self.log_path.exists() {
            return Ok(Vec::new());
        }

        let content = std::fs::read_to_string(&self.log_path)
            .map_err(|e| MemoryError::Io(e.to_string()))?;

        let mut entries = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<MemoryCandidate>(trimmed) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    fn rewrite_log(&self, entries: &[MemoryCandidate]) -> Result<(), MemoryError> {
        if let Some(parent) = self.log_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MemoryError::Io(e.to_string()))?;
        }

        let mut content = String::new();
        for entry in entries {
            let json = serde_json::to_string(entry)
                .map_err(|e| MemoryError::Serialization(e.to_string()))?;
            content.push_str(&json);
            content.push('\n');
        }

        std::fs::write(&self.log_path, &content)
            .map_err(|e| MemoryError::Io(e.to_string()))?;

        Ok(())
    }

    fn load_index(log_path: &Path) -> (u64, Vec<EntryFingerprint>) {
        let content = match std::fs::read_to_string(log_path) {
            Ok(c) => c,
            Err(_) => return (1, Vec::new()),
        };

        let mut max_id = 0u64;
        let mut fingerprints = Vec::new();

        for line in content.lines() {
            if let Ok(entry) = serde_json::from_str::<MemoryCandidate>(line.trim()) {
                if entry.id > max_id {
                    max_id = entry.id;
                }
                if entry.status == CandidateStatus::Committed {
                    fingerprints.push(EntryFingerprint::from_text(&entry.text));
                }
            }
        }

        (max_id + 1, fingerprints)
    }
}

/// Statistics from the memory log.
#[derive(Debug, Clone)]
pub struct MemoryLogStats {
    pub total_entries: usize,
    pub pending: usize,
    pub committed: usize,
    pub rejected_secret: usize,
    pub rejected_duplicate: usize,
    pub fingerprint_index_size: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_and_process_pipeline() {
        let dir = tempdir().unwrap();
        let mut log = MemoryLog::open(dir.path());

        // Append candidates
        log.append_candidate("Use Tokio for async runtime", "architecture", "session-1", 0.8).unwrap();
        log.append_candidate("Prefer explicit error types over anyhow", "preferences", "session-1", 0.7).unwrap();

        // Process pipeline
        let (committed, rejected) = log.process_pending().unwrap();
        assert_eq!(committed, 2);
        assert_eq!(rejected, 0);

        // Stats check
        let stats = log.stats();
        assert_eq!(stats.committed, 2);
        assert_eq!(stats.fingerprint_index_size, 2);
    }

    #[test]
    fn duplicate_rejection() {
        let dir = tempdir().unwrap();
        let mut log = MemoryLog::open(dir.path());

        log.append_candidate("Use Tokio for the async runtime in this project", "architecture", "s1", 0.8).unwrap();
        let (c1, _) = log.process_pending().unwrap();
        assert_eq!(c1, 1);

        // Near-duplicate (very similar wording)
        log.append_candidate("Use Tokio for async runtime in the project", "architecture", "s2", 0.7).unwrap();
        let (c2, r2) = log.process_pending().unwrap();
        assert_eq!(c2, 0);
        assert_eq!(r2, 1);
    }

    #[test]
    fn secret_rejection() {
        let dir = tempdir().unwrap();
        let mut log = MemoryLog::open(dir.path());

        log.append_candidate(
            "API key is sk-ant-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGH",
            "project", "s1", 0.5
        ).unwrap();
        let (committed, rejected) = log.process_pending().unwrap();
        assert_eq!(committed, 0);
        assert_eq!(rejected, 1);

        let stats = log.stats();
        assert_eq!(stats.rejected_secret, 1);
    }

    #[test]
    fn project_to_memory_document() {
        let dir = tempdir().unwrap();
        let mem_path = dir.path().join("MEMORY.md");
        let mut doc = crate::MemoryDocument::new_empty(&mem_path);

        let mut log = MemoryLog::open(dir.path());
        log.append_candidate("Always run clippy before commits", "workflow", "s1", 0.9).unwrap();
        log.append_candidate("Database schema lives in db/migrations/", "project", "s1", 0.8).unwrap();
        log.process_pending().unwrap();

        let projected = log.project_and_compact(&mut doc).unwrap();
        assert_eq!(projected, 2);
        assert!(doc.body.contains("Always run clippy"));
        assert!(doc.body.contains("Database schema"));
    }

    #[test]
    fn jaccard_similarity() {
        let a = EntryFingerprint::from_text("use tokio for async runtime");
        let b = EntryFingerprint::from_text("use tokio for the async runtime");
        assert!(a.similarity(&b) > 0.6);

        let c = EntryFingerprint::from_text("prefer explicit error handling");
        assert!(a.similarity(&c) < 0.3);
    }

    #[test]
    fn compaction() {
        let dir = tempdir().unwrap();
        let mut log = MemoryLog::open(dir.path());
        log.max_log_entries = 5;

        for i in 0..10 {
            log.append_candidate(&format!("Fact number {i}"), "project", "s1", 0.5).unwrap();
        }
        log.process_pending().unwrap();
        log.compact().unwrap();

        let stats = log.stats();
        assert!(stats.total_entries <= 5);
    }

    #[test]
    fn index_existing_memory() {
        let dir = tempdir().unwrap();
        let mut log = MemoryLog::open(dir.path());

        let existing_body = "## project\n\n- Use Tokio for async runtime\n- Database is PostgreSQL\n";
        log.index_existing_memory(existing_body);

        // Now a near-duplicate should be rejected
        log.append_candidate("Use Tokio for the async runtime", "project", "s1", 0.8).unwrap();
        let (committed, rejected) = log.process_pending().unwrap();
        assert_eq!(committed, 0);
        assert_eq!(rejected, 1);
    }
}
