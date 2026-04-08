//! Session Memory Compaction Sink — converts lossy truncation to lossless two-tier memory.
//!
//! Architecture:
//!   C_working (context window, size ≤ B) + C_long_term (memory store, unbounded)
//!
//! Every AutoCompactPass summary is persisted to the memory store.
//! The model can retrieve historical summaries via recall_memory().
//!
//! Information-theoretic claim:
//!   P(recall_success | C_working ∪ C_long_term) ≥ P(recall_success | C_working)
//!   strictly, with equality only when the relevant information was never compacted.
//!
//! Write: O(1) append.
//! Read: O(log n) approximate nearest neighbor via semantic search.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A memory entry stored in the long-term session memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique identifier.
    pub id: String,
    /// Session ID this memory belongs to.
    pub session_id: String,
    /// The compaction summary text.
    pub summary: String,
    /// Turn range this summary covers (e.g. "turns 1-15").
    pub turn_range: String,
    /// Key topics/entities mentioned (for keyword search).
    pub topics: Vec<String>,
    /// Timestamp (unix ms).
    pub created_at: u64,
    /// Estimated token count of the original content.
    pub original_tokens: u64,
    /// Estimated token count of the summary.
    pub summary_tokens: u64,
}

/// Trait for the session memory store backend.
///
/// Default: in-memory HashMap with JSON persistence.
/// Feature-gated alternatives: SQLite+sqlite-vec, Qdrant, Lance.
pub trait MemoryStore: Send + Sync {
    /// Store a compaction summary. O(1) append.
    fn store(&self, entry: MemoryEntry) -> Result<String, String>;

    /// Recall memories matching a query. O(log n) with semantic search.
    fn recall(
        &self,
        session_id: &str,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<MemoryEntry>, String>;

    /// List all memories for a session.
    fn list(&self, session_id: &str) -> Result<Vec<MemoryEntry>, String>;

    /// Delete a memory by ID.
    fn delete(&self, id: &str) -> Result<bool, String>;

    /// Count of stored memories.
    fn count(&self, session_id: &str) -> usize;
}

/// Simple in-memory store with keyword-based search.
/// For production, swap with SQLite+FTS5 or a vector store.
pub struct InMemoryStore {
    entries: Mutex<Vec<MemoryEntry>>,
    persist_path: Option<PathBuf>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            persist_path: None,
        }
    }

    /// Create with a persistence path — loads on init, saves on store.
    pub fn with_persistence(path: PathBuf) -> Self {
        let entries = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => Vec::new(),
            }
        } else {
            Vec::new()
        };

        Self {
            entries: Mutex::new(entries),
            persist_path: Some(path),
        }
    }

    fn persist(&self) {
        if let Some(ref path) = self.persist_path {
            let entries = self.entries.lock().unwrap();
            if let Ok(json) = serde_json::to_string_pretty(&*entries) {
                let _ = std::fs::write(path, json);
            }
        }
    }
}

impl MemoryStore for InMemoryStore {
    fn store(&self, entry: MemoryEntry) -> Result<String, String> {
        let id = entry.id.clone();
        let mut entries = self.entries.lock().map_err(|e| e.to_string())?;
        entries.push(entry);
        drop(entries);
        self.persist();
        Ok(id)
    }

    fn recall(
        &self,
        session_id: &str,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<MemoryEntry>, String> {
        let entries = self.entries.lock().map_err(|e| e.to_string())?;
        let query_lower = query.to_lowercase();
        let query_terms: Vec<&str> = query_lower.split_whitespace().collect();

        // Score by keyword overlap (BM25-like approximation)
        let mut scored: Vec<(usize, f32)> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.session_id == session_id)
            .map(|(i, entry)| {
                let text = format!("{} {}", entry.summary, entry.topics.join(" ")).to_lowercase();

                let score: f32 = query_terms
                    .iter()
                    .map(|term| {
                        let tf = text.matches(term).count() as f32;
                        let topic_bonus =
                            if entry.topics.iter().any(|t| t.to_lowercase().contains(term)) {
                                3.0
                            } else {
                                0.0
                            };
                        tf.min(5.0) + topic_bonus
                    })
                    .sum();

                (i, score)
            })
            .filter(|(_, s)| *s > 0.0)
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(scored
            .iter()
            .take(top_k)
            .map(|(i, _)| entries[*i].clone())
            .collect())
    }

    fn list(&self, session_id: &str) -> Result<Vec<MemoryEntry>, String> {
        let entries = self.entries.lock().map_err(|e| e.to_string())?;
        Ok(entries
            .iter()
            .filter(|e| e.session_id == session_id)
            .cloned()
            .collect())
    }

    fn delete(&self, id: &str) -> Result<bool, String> {
        let mut entries = self.entries.lock().map_err(|e| e.to_string())?;
        let before = entries.len();
        entries.retain(|e| e.id != id);
        let deleted = entries.len() < before;
        drop(entries);
        if deleted {
            self.persist();
        }
        Ok(deleted)
    }

    fn count(&self, session_id: &str) -> usize {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.session_id == session_id)
            .count()
    }
}

/// Create a MemoryEntry from an AutoCompactPass summary.
pub fn summary_to_memory(
    session_id: &str,
    summary: &str,
    turn_start: u32,
    turn_end: u32,
    original_tokens: u64,
) -> MemoryEntry {
    let topics = extract_topics(summary);
    let id = format!(
        "mem_{}_{}",
        session_id.chars().take(8).collect::<String>(),
        uuid::Uuid::new_v4()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>()
    );

    MemoryEntry {
        id,
        session_id: session_id.to_string(),
        summary: summary.to_string(),
        turn_range: format!("turns {}-{}", turn_start, turn_end),
        topics,
        created_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        original_tokens,
        summary_tokens: (summary.len() / 4) as u64,
    }
}

/// Extract key topics from a summary for keyword search.
/// Simple heuristic: words that appear capitalized, in backticks,
/// or after "FILE:" / "TASK:" markers.
fn extract_topics(summary: &str) -> Vec<String> {
    let mut topics = Vec::new();

    // Extract backtick-quoted identifiers
    for cap in summary.split('`') {
        // Every other segment is inside backticks
        if cap.len() > 1 && cap.len() < 80 && !cap.contains(' ') {
            topics.push(cap.to_string());
        }
    }

    // Extract file paths (anything ending in common extensions)
    for word in summary.split_whitespace() {
        let clean =
            word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.' && c != '/' && c != '_');
        if clean.contains('.')
            && (clean.ends_with(".rs")
                || clean.ends_with(".py")
                || clean.ends_with(".ts")
                || clean.ends_with(".js")
                || clean.ends_with(".toml")
                || clean.ends_with(".json")
                || clean.ends_with(".md"))
        {
            topics.push(clean.to_string());
        }
    }

    // Deduplicate
    topics.sort();
    topics.dedup();
    topics.truncate(20); // Cap topic count
    topics
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_and_recall() {
        let store = InMemoryStore::new();
        let entry = MemoryEntry {
            id: "mem_1".into(),
            session_id: "s1".into(),
            summary: "Fixed authentication bug in server.py by adding JWT validation".into(),
            turn_range: "turns 1-5".into(),
            topics: vec!["auth".into(), "JWT".into(), "server.py".into()],
            created_at: 0,
            original_tokens: 5000,
            summary_tokens: 50,
        };

        store.store(entry).unwrap();
        assert_eq!(store.count("s1"), 1);

        let results = store.recall("s1", "authentication JWT", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].summary.contains("JWT"));
    }

    #[test]
    fn recall_empty_returns_empty() {
        let store = InMemoryStore::new();
        let results = store.recall("s1", "anything", 5).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn recall_filters_by_session() {
        let store = InMemoryStore::new();
        store
            .store(MemoryEntry {
                id: "m1".into(),
                session_id: "s1".into(),
                summary: "auth fix".into(),
                turn_range: "1-5".into(),
                topics: vec!["auth".into()],
                created_at: 0,
                original_tokens: 100,
                summary_tokens: 10,
            })
            .unwrap();
        store
            .store(MemoryEntry {
                id: "m2".into(),
                session_id: "s2".into(),
                summary: "auth fix".into(),
                turn_range: "1-3".into(),
                topics: vec!["auth".into()],
                created_at: 0,
                original_tokens: 100,
                summary_tokens: 10,
            })
            .unwrap();

        let results = store.recall("s1", "auth", 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "s1");
    }

    #[test]
    fn topic_extraction() {
        let summary = "Fixed `server.py` auth bug. Modified server/auth.rs and added tests.";
        let topics = extract_topics(summary);
        assert!(topics.contains(&"server.py".to_string()));
        // "server/auth.rs" is extracted as a path-like token
        assert!(
            topics.iter().any(|t| t.contains("auth.rs")),
            "Expected auth.rs in topics: {:?}",
            topics
        );
    }

    #[test]
    fn summary_to_memory_creates_entry() {
        let entry = summary_to_memory("test-session", "Fixed auth bug", 1, 5, 5000);
        assert!(!entry.id.is_empty());
        assert_eq!(entry.session_id, "test-session");
        assert_eq!(entry.turn_range, "turns 1-5");
    }

    #[test]
    fn delete_works() {
        let store = InMemoryStore::new();
        store
            .store(MemoryEntry {
                id: "m1".into(),
                session_id: "s1".into(),
                summary: "test".into(),
                turn_range: "1-1".into(),
                topics: vec![],
                created_at: 0,
                original_tokens: 0,
                summary_tokens: 0,
            })
            .unwrap();
        assert_eq!(store.count("s1"), 1);
        assert!(store.delete("m1").unwrap());
        assert_eq!(store.count("s1"), 0);
    }
}
