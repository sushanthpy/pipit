//! Content-Addressed Tool Result Store (Architecture Task 3)
//!
//! A blob store for large tool results. The LLM context receives compact
//! references plus summaries, while the full payload is stored under
//! `hash(content)` in a local artifact directory.
//!
//! Integrates with the proof/evidence system: every large tool result
//! becomes a first-class evidence artifact.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Maximum inline size before redirecting to blob store.
const DEFAULT_INLINE_THRESHOLD: usize = 8_000;

/// Descriptor stored inline in the context instead of the full content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobDescriptor {
    /// Content hash (hex-encoded).
    pub hash: String,
    /// Original content size in bytes.
    pub size: usize,
    /// MIME type hint.
    pub mime: String,
    /// One-line summary of the content.
    pub summary: String,
    /// Tool name that produced this result.
    pub tool_name: String,
    /// Tool call ID.
    pub call_id: String,
}

impl BlobDescriptor {
    /// Render as inline text for the LLM context.
    pub fn as_context_text(&self) -> String {
        format!(
            "[Stored result: {} bytes, hash={}]\nSummary: {}",
            self.size,
            &self.hash[..12],
            self.summary
        )
    }
}

/// The content-addressed blob store.
pub struct BlobStore {
    /// Root directory for blob storage.
    store_dir: PathBuf,
    /// Inline threshold (bytes). Content smaller than this is kept inline.
    inline_threshold: usize,
    /// In-memory index: hash → descriptor.
    index: HashMap<String, BlobDescriptor>,
    /// LRU generation tracker for GC.
    generation: u64,
    /// Hash → last-accessed generation.
    access_gen: HashMap<String, u64>,
}

impl BlobStore {
    /// Create or open a blob store at the given directory.
    pub fn open(store_dir: PathBuf) -> Result<Self, BlobStoreError> {
        std::fs::create_dir_all(&store_dir)?;

        // Rebuild index from existing blobs
        let mut index = HashMap::new();
        let meta_dir = store_dir.join("meta");
        if meta_dir.exists() {
            for entry in std::fs::read_dir(&meta_dir)? {
                let entry = entry?;
                if entry.path().extension().map_or(false, |e| e == "json") {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(desc) = serde_json::from_str::<BlobDescriptor>(&content) {
                            index.insert(desc.hash.clone(), desc);
                        }
                    }
                }
            }
        }

        Ok(Self {
            store_dir,
            inline_threshold: DEFAULT_INLINE_THRESHOLD,
            index,
            generation: 0,
            access_gen: HashMap::new(),
        })
    }

    /// Set the inline threshold.
    pub fn with_threshold(mut self, threshold: usize) -> Self {
        self.inline_threshold = threshold;
        self
    }

    /// Store content if it exceeds the inline threshold.
    /// Returns Some(descriptor) if stored, None if content is small enough to inline.
    pub fn store_if_large(
        &mut self,
        content: &str,
        tool_name: &str,
        call_id: &str,
        summary: &str,
    ) -> Result<Option<BlobDescriptor>, BlobStoreError> {
        if content.len() <= self.inline_threshold {
            return Ok(None);
        }

        let hash = content_hash(content);

        // Check if already stored (content-addressed dedup)
        if self.index.contains_key(&hash) {
            self.touch(&hash);
            return Ok(Some(self.index[&hash].clone()));
        }

        // Write blob
        let blob_dir = self.store_dir.join("blobs");
        std::fs::create_dir_all(&blob_dir)?;
        let blob_path = blob_dir.join(&hash);
        std::fs::write(&blob_path, content)?;

        // Write metadata
        let meta_dir = self.store_dir.join("meta");
        std::fs::create_dir_all(&meta_dir)?;
        let descriptor = BlobDescriptor {
            hash: hash.clone(),
            size: content.len(),
            mime: guess_mime(content),
            summary: summary.to_string(),
            tool_name: tool_name.to_string(),
            call_id: call_id.to_string(),
        };
        let meta_path = meta_dir.join(format!("{}.json", &hash));
        let meta_json = serde_json::to_string_pretty(&descriptor)
            .map_err(|e| BlobStoreError::Serialization(e.to_string()))?;
        std::fs::write(&meta_path, meta_json)?;

        self.index.insert(hash.clone(), descriptor.clone());
        self.touch(&hash);

        Ok(Some(descriptor))
    }

    /// Retrieve full content by hash.
    pub fn retrieve(&mut self, hash: &str) -> Result<String, BlobStoreError> {
        let blob_path = self.store_dir.join("blobs").join(hash);
        if !blob_path.exists() {
            return Err(BlobStoreError::NotFound(hash.to_string()));
        }
        self.touch(hash);
        std::fs::read_to_string(&blob_path).map_err(BlobStoreError::from)
    }

    /// Check if a blob exists.
    pub fn contains(&self, hash: &str) -> bool {
        self.index.contains_key(hash)
    }

    /// Get the descriptor for a blob.
    pub fn descriptor(&self, hash: &str) -> Option<&BlobDescriptor> {
        self.index.get(hash)
    }

    /// List all stored blob descriptors.
    pub fn list(&self) -> Vec<&BlobDescriptor> {
        self.index.values().collect()
    }

    /// Total bytes stored.
    pub fn total_bytes(&self) -> usize {
        self.index.values().map(|d| d.size).sum()
    }

    /// Number of blobs stored.
    pub fn count(&self) -> usize {
        self.index.len()
    }

    /// Garbage-collect blobs not accessed in the last `keep_generations` generations.
    pub fn gc(&mut self, keep_generations: u64) -> Result<usize, BlobStoreError> {
        let cutoff = self.generation.saturating_sub(keep_generations);
        let to_remove: Vec<String> = self
            .access_gen
            .iter()
            .filter(|(_, generation)| **generation < cutoff)
            .map(|(hash, _)| hash.clone())
            .collect();

        let mut removed = 0;
        for hash in &to_remove {
            let blob_path = self.store_dir.join("blobs").join(hash);
            let meta_path = self.store_dir.join("meta").join(format!("{}.json", hash));
            let _ = std::fs::remove_file(&blob_path);
            let _ = std::fs::remove_file(&meta_path);
            self.index.remove(hash);
            self.access_gen.remove(hash);
            removed += 1;
        }

        Ok(removed)
    }

    /// Advance the generation counter (called once per turn).
    pub fn advance_generation(&mut self) {
        self.generation += 1;
    }

    fn touch(&mut self, hash: &str) {
        self.access_gen.insert(hash.to_string(), self.generation);
    }
}

/// Compute a content-addressable SHA-256 hash (replacing DefaultHasher).
fn content_hash(content: &str) -> String {
    let digest = Sha256::digest(content.as_bytes());
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Guess MIME type from content heuristics.
fn guess_mime(content: &str) -> String {
    if content.starts_with('{') || content.starts_with('[') {
        "application/json".to_string()
    } else if content.contains("diff --git") || content.contains("---") {
        "text/x-diff".to_string()
    } else {
        "text/plain".to_string()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BlobStoreError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Blob not found: {0}")]
    NotFound(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_content_stays_inline() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path().to_path_buf()).unwrap();
        let result = store
            .store_if_large("small", "bash", "call-1", "summary")
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn large_content_stored_and_retrievable() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path().to_path_buf())
            .unwrap()
            .with_threshold(10);

        let content = "x".repeat(100);
        let desc = store
            .store_if_large(&content, "grep", "call-2", "100 matches")
            .unwrap()
            .unwrap();

        assert_eq!(desc.size, 100);
        assert!(store.contains(&desc.hash));

        let retrieved = store.retrieve(&desc.hash).unwrap();
        assert_eq!(retrieved, content);
    }

    #[test]
    fn content_deduplication() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = BlobStore::open(dir.path().to_path_buf())
            .unwrap()
            .with_threshold(10);

        let content = "x".repeat(100);
        let d1 = store
            .store_if_large(&content, "bash", "c1", "s1")
            .unwrap()
            .unwrap();
        let d2 = store
            .store_if_large(&content, "bash", "c2", "s2")
            .unwrap()
            .unwrap();

        assert_eq!(d1.hash, d2.hash);
        assert_eq!(store.count(), 1); // Only one blob
    }
}
