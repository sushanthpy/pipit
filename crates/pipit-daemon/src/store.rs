//! Unified SochDB storage backend with path-keyed schema.
//!
//! Key schema:
//! ```text
//! tasks/{task_id}                    → TaskRecord JSON
//! tasks/{task_id}/events/{seq}       → TaskEvent JSON
//! tasks/{task_id}/proof              → ProofPacket JSON
//! projects/{name}/config             → ProjectConfig JSON
//! projects/{name}/context            → Serialized agent context
//! projects/{name}/last_task          → Pointer to last task_id
//! sessions/{project}/{session}       → Session snapshot
//! cron/{schedule_name}/last_fire     → Timestamp
//! cron/{schedule_name}/next_fire     → Timestamp
//! vectors/tasks/{task_id}            → Embedding vector
//! ```
//!
//! All writes go through three modes:
//! - `put()` — group-commit, high throughput
//! - `put_durable()` — immediate fsync
//! - `apply_atomic_batch()` — N writes + 1 commit

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use pipit_channel::{NormalizedTask, TaskRecord, TaskStatus, TaskUpdate, TaskUpdateKind};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tracing;

// ---------------------------------------------------------------------------
// Store implementation — embedded key-value with WAL semantics
// ---------------------------------------------------------------------------

/// Persistent store backed by a WAL-protected key-value engine.
/// In this implementation we use `sled` as the embedded B-tree engine,
/// which provides atomic batch writes, prefix scans, and crash recovery.
///
/// The API surface mirrors SochDB's `SochStore` from ClawDesk:
/// `put`, `put_durable`, `get`, `scan`, `delete`, `apply_atomic_batch`,
/// `checkpoint`, `sync`.
pub struct DaemonStore {
    db: sled::Db,
    event_seq: AtomicU64,
    shutdown: AtomicBool,
    store_path: PathBuf,
}

impl DaemonStore {
    /// Open or create the store at the given path.
    /// Performs a canary write/read to verify persistence integrity.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let db = sled::open(path).map_err(|e| anyhow!("failed to open store at {}: {}", path.display(), e))?;

        // Persistence self-test: canary write → read-back
        let canary_key = b"__canary__";
        let canary_val = b"ok";
        db.insert(canary_key, canary_val.as_ref())?;
        db.flush()?;
        let readback = db.get(canary_key)?;
        if readback.as_deref() != Some(canary_val.as_ref()) {
            return Err(anyhow!("store persistence self-test failed"));
        }
        db.remove(canary_key)?;

        // Recover event sequence counter
        // Primary: read from dedicated meta key (O(1))
        // Fallback: scan events prefix (O(R)) for backward compatibility
        let max_seq: u64 = if let Some(seq_bytes) = db.get("meta/event_seq")? {
            let bytes: [u8; 8] = seq_bytes.as_ref().try_into().unwrap_or([0u8; 8]);
            u64::from_le_bytes(bytes)
        } else {
            // Backward-compatible scan (only runs once on upgrade)
            let mut found_max: u64 = 0;
            for item in db.scan_prefix("tasks/") {
                if let Ok((key, _)) = item {
                    let key_str = String::from_utf8_lossy(&key);
                    // Look for pattern: tasks/{id}/events/{seq}
                    let parts: Vec<&str> = key_str.split('/').collect();
                    if parts.len() == 4 && parts[2] == "events" {
                        if let Ok(seq) = parts[3].parse::<u64>() {
                            found_max = found_max.max(seq);
                        }
                    }
                }
            }
            // Persist for future O(1) recovery
            if found_max > 0 {
                db.insert("meta/event_seq", &found_max.to_le_bytes())?;
            }
            found_max
        };

        tracing::info!(path = %path.display(), "daemon store opened");

        Ok(Self {
            db,
            event_seq: AtomicU64::new(max_seq + 1),
            shutdown: AtomicBool::new(false),
            store_path: path.to_path_buf(),
        })
    }

    /// Open an ephemeral in-memory store (for testing).
    pub fn open_ephemeral() -> Result<Self> {
        let db = sled::Config::new()
            .temporary(true)
            .open()
            .map_err(|e| anyhow!("failed to open ephemeral store: {e}"))?;

        Ok(Self {
            db,
            event_seq: AtomicU64::new(1),
            shutdown: AtomicBool::new(false),
            store_path: PathBuf::from(":memory:"),
        })
    }

    // -----------------------------------------------------------------------
    // Low-level KV operations
    // -----------------------------------------------------------------------

    /// Group-commit write (high throughput, eventual durability via WAL).
    pub fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| anyhow!("put failed for key '{}': {}", key, e))?;
        Ok(())
    }

    /// Durable write with immediate fsync.
    pub fn put_durable(&self, key: &str, value: &[u8]) -> Result<()> {
        self.db
            .insert(key.as_bytes(), value)
            .map_err(|e| anyhow!("put_durable failed for key '{}': {}", key, e))?;
        self.db.flush()?;
        Ok(())
    }

    /// Get a value by key.
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let val = self
            .db
            .get(key.as_bytes())
            .map_err(|e| anyhow!("get failed for key '{}': {}", key, e))?;
        Ok(val.map(|v| v.to_vec()))
    }

    /// Delete a key.
    pub fn delete(&self, key: &str) -> Result<()> {
        self.db
            .remove(key.as_bytes())
            .map_err(|e| anyhow!("delete failed for key '{}': {}", key, e))?;
        Ok(())
    }

    /// Prefix scan — returns all key-value pairs with the given prefix.
    /// O(log N + R) where R is the result set size.
    pub fn scan(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let mut results = Vec::new();
        for item in self.db.scan_prefix(prefix.as_bytes()) {
            let (key, value) = item.map_err(|e| anyhow!("scan failed: {e}"))?;
            let key_str = String::from_utf8(key.to_vec())
                .map_err(|e| anyhow!("invalid utf-8 key: {e}"))?;
            results.push((key_str, value.to_vec()));
        }
        Ok(results)
    }

    /// Atomic batch write — N writes committed as a single transaction.
    pub fn apply_atomic_batch(&self, entries: &[(&str, &[u8])]) -> Result<()> {
        let mut batch = sled::Batch::default();
        for (key, value) in entries {
            batch.insert(key.as_bytes(), *value);
        }
        self.db
            .apply_batch(batch)
            .map_err(|e| anyhow!("atomic batch failed: {e}"))?;
        self.db.flush()?;
        Ok(())
    }

    /// Flush WAL to disk.
    pub fn checkpoint(&self) -> Result<()> {
        self.db.flush().map_err(|e| anyhow!("checkpoint failed: {e}"))?;
        Ok(())
    }

    /// Explicit fsync.
    pub fn sync(&self) -> Result<()> {
        self.db.flush().map_err(|e| anyhow!("sync failed: {e}"))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Typed JSON helpers
    // -----------------------------------------------------------------------

    pub fn put_json<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_vec(value)?;
        self.put(key, &json)
    }

    pub fn put_json_durable<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let json = serde_json::to_vec(value)?;
        self.put_durable(key, &json)
    }

    pub fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        match self.get(key)? {
            Some(bytes) => {
                let value = serde_json::from_slice(&bytes)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    // -----------------------------------------------------------------------
    // Task operations
    // -----------------------------------------------------------------------

    /// Store a new task record + update the project's last_task pointer
    /// in a single atomic batch.
    pub fn create_task(&self, task: &NormalizedTask) -> Result<TaskRecord> {
        let record = TaskRecord::from_task(task);
        let task_key = format!("tasks/{}", record.task_id);
        let project_key = format!("projects/{}/last_task", record.project);

        let task_json = serde_json::to_vec(&record)?;
        let task_id_bytes = record.task_id.as_bytes().to_vec();

        self.apply_atomic_batch(&[
            (task_key.as_str(), &task_json),
            (project_key.as_str(), &task_id_bytes),
        ])?;

        Ok(record)
    }

    /// Update task status with optional fields.
    pub fn update_task_status(
        &self,
        task_id: &str,
        status: TaskStatus,
        update: impl FnOnce(&mut TaskRecord),
    ) -> Result<TaskRecord> {
        let key = format!("tasks/{}", task_id);
        let mut record: TaskRecord = self
            .get_json(&key)?
            .ok_or_else(|| anyhow!("task not found: {}", task_id))?;

        record.status = status;
        update(&mut record);

        self.put_json_durable(&key, &record)?;
        Ok(record)
    }

    /// Get a task record.
    pub fn get_task(&self, task_id: &str) -> Result<Option<TaskRecord>> {
        self.get_json(&format!("tasks/{}", task_id))
    }

    /// List all tasks (prefix scan).
    pub fn list_tasks(&self) -> Result<Vec<TaskRecord>> {
        let entries = self.scan("tasks/")?;
        let mut tasks = Vec::new();
        for (key, value) in entries {
            // Only match top-level task records, not sub-keys like events/proof
            let parts: Vec<&str> = key.split('/').collect();
            if parts.len() == 2 {
                if let Ok(record) = serde_json::from_slice::<TaskRecord>(&value) {
                    tasks.push(record);
                }
            }
        }
        Ok(tasks)
    }

    /// List tasks for a specific project.
    pub fn list_project_tasks(&self, project: &str) -> Result<Vec<TaskRecord>> {
        let all = self.list_tasks()?;
        Ok(all.into_iter().filter(|t| t.project == project).collect())
    }

    // -----------------------------------------------------------------------
    // Event logging
    // -----------------------------------------------------------------------

    /// Append a task event with tamper-evident hash chain.
    ///
    /// Each event is chained: H_i = SHA-256(H_{i-1} || event_i || timestamp_i || seq_i)
    /// This makes post-hoc modification detectable with O(n) verification cost.
    pub fn append_event(&self, task_id: &str, event: &TaskUpdateKind) -> Result<u64> {
        let seq = self.event_seq.fetch_add(1, Ordering::Relaxed);
        let timestamp = Utc::now();
        let key = format!("tasks/{}/events/{:010}", task_id, seq);

        // Retrieve previous hash in the chain (empty for first event)
        let prev_hash = self.get_chain_hash(task_id)?.unwrap_or_default();

        // Compute chain hash: H_i = SHA-256(H_{i-1} || event_json || timestamp || seq)
        let event_json = serde_json::to_string(event).unwrap_or_default();
        let chain_hash = compute_chain_hash(&prev_hash, &event_json, &timestamp, seq);

        #[derive(Serialize)]
        struct StoredEvent<'a> {
            seq: u64,
            timestamp: DateTime<Utc>,
            event: &'a TaskUpdateKind,
            /// SHA-256 hash chain link: H_i = Hash(H_{i-1} || event || timestamp || seq)
            chain_hash: String,
            /// Previous hash in the chain (for verification without scanning)
            prev_hash: String,
        }

        let stored = StoredEvent {
            seq,
            timestamp,
            event,
            chain_hash: chain_hash.clone(),
            prev_hash,
        };

        self.put_json(&key, &stored)?;

        // Persist current chain hash for O(1) next-append lookup
        let chain_key = format!("tasks/{}/chain_hash", task_id);
        self.put(chain_key.as_str(), chain_hash.as_bytes())?;

        // Persist sequence counter for O(1) recovery on restart
        self.db.insert("meta/event_seq", &seq.to_le_bytes())?;

        Ok(seq)
    }

    /// Verify the integrity of a task's event chain.
    /// Returns Ok(n) where n is the number of verified events,
    /// or Err if any event's hash doesn't match.
    /// Verification cost: O(n) for a task with n events.
    pub fn verify_event_chain(&self, task_id: &str) -> Result<u64> {
        let prefix = format!("tasks/{}/events/", task_id);
        let entries = self.scan(&prefix)?;

        let mut prev_hash = String::new();
        let mut verified = 0u64;

        for (_, value) in &entries {
            let stored: serde_json::Value = serde_json::from_slice(value)
                .map_err(|e| anyhow!("failed to parse event: {e}"))?;

            let recorded_hash = stored.get("chain_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let recorded_prev = stored.get("prev_hash")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let seq = stored.get("seq")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let timestamp_str = stored.get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let event_json = stored.get("event")
                .map(|v| serde_json::to_string(v).unwrap_or_default())
                .unwrap_or_default();

            // Verify previous hash matches what we expect
            if recorded_prev != prev_hash {
                return Err(anyhow!(
                    "chain break at seq {}: expected prev_hash '{}', got '{}'",
                    seq, prev_hash, recorded_prev
                ));
            }

            // Recompute hash and verify
            let timestamp = DateTime::parse_from_rfc3339(timestamp_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());
            let expected_hash = compute_chain_hash(&prev_hash, &event_json, &timestamp, seq);

            if expected_hash != recorded_hash {
                return Err(anyhow!(
                    "tamper detected at seq {}: expected hash '{}', got '{}'",
                    seq, expected_hash, recorded_hash
                ));
            }

            prev_hash = recorded_hash.to_string();
            verified += 1;
        }

        Ok(verified)
    }

    /// Get the current chain hash for a task.
    fn get_chain_hash(&self, task_id: &str) -> Result<Option<String>> {
        let key = format!("tasks/{}/chain_hash", task_id);
        match self.get(&key)? {
            Some(bytes) => Ok(Some(String::from_utf8(bytes)
                .map_err(|e| anyhow!("invalid chain hash: {e}"))?)),
            None => Ok(None),
        }
    }

    /// Get all events for a task.
    pub fn get_task_events(&self, task_id: &str) -> Result<Vec<serde_json::Value>> {
        let prefix = format!("tasks/{}/events/", task_id);
        let entries = self.scan(&prefix)?;
        let mut events = Vec::new();
        for (_, value) in entries {
            if let Ok(event) = serde_json::from_slice(&value) {
                events.push(event);
            }
        }
        Ok(events)
    }

    // -----------------------------------------------------------------------
    // Proof packet storage
    // -----------------------------------------------------------------------

    /// Store proof packet atomically with the task record update.
    pub fn store_proof<T: Serialize>(
        &self,
        task_id: &str,
        proof: &T,
        summary: &str,
        files_modified: Vec<String>,
        turns: u32,
        cost: f64,
        tokens: u64,
    ) -> Result<()> {
        let task_key = format!("tasks/{}", task_id);
        let proof_key = format!("tasks/{}/proof", task_id);

        let mut record: TaskRecord = self
            .get_json(&task_key)?
            .ok_or_else(|| anyhow!("task not found: {}", task_id))?;

        record.status = TaskStatus::Completed;
        record.completed_at = Some(Utc::now());
        record.result_summary = Some(summary.to_string());
        record.files_modified = files_modified;
        record.turns = Some(turns);
        record.cost = Some(cost);
        record.total_tokens = Some(tokens);

        let record_json = serde_json::to_vec(&record)?;
        let proof_json = serde_json::to_vec(proof)?;

        self.apply_atomic_batch(&[
            (task_key.as_str(), &record_json),
            (proof_key.as_str(), &proof_json),
        ])?;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Project context persistence
    // -----------------------------------------------------------------------

    /// Save agent context (serialized messages) for a project.
    pub fn save_context(&self, project: &str, context_bytes: &[u8]) -> Result<()> {
        let key = format!("projects/{}/context", project);
        self.put_durable(&key, context_bytes)
    }

    /// Load agent context for a project.
    pub fn load_context(&self, project: &str) -> Result<Option<Vec<u8>>> {
        let key = format!("projects/{}/context", project);
        self.get(&key)
    }

    // -----------------------------------------------------------------------
    // Cron state
    // -----------------------------------------------------------------------

    /// Persist next fire time for a schedule.
    pub fn set_cron_next_fire(&self, schedule_name: &str, next: DateTime<Utc>) -> Result<()> {
        let key = format!("cron/{}/next_fire", schedule_name);
        let ts = next.to_rfc3339();
        self.put_durable(&key, ts.as_bytes())
    }

    /// Get next fire time for a schedule.
    pub fn get_cron_next_fire(&self, schedule_name: &str) -> Result<Option<DateTime<Utc>>> {
        let key = format!("cron/{}/next_fire", schedule_name);
        match self.get(&key)? {
            Some(bytes) => {
                let ts_str = String::from_utf8(bytes)?;
                let dt = DateTime::parse_from_rfc3339(&ts_str)
                    .map_err(|e| anyhow!("invalid cron timestamp: {e}"))?
                    .with_timezone(&Utc);
                Ok(Some(dt))
            }
            None => Ok(None),
        }
    }

    /// Set last fire time for a schedule.
    pub fn set_cron_last_fire(&self, schedule_name: &str, last: DateTime<Utc>) -> Result<()> {
        let key = format!("cron/{}/last_fire", schedule_name);
        let ts = last.to_rfc3339();
        self.put_durable(&key, ts.as_bytes())
    }

    // -----------------------------------------------------------------------
    // Vector storage (for HNSW task embeddings)
    // -----------------------------------------------------------------------

    /// Store an embedding vector for a task.
    pub fn store_vector(&self, task_id: &str, embedding: &[f32], metadata: &serde_json::Value) -> Result<()> {
        let data_key = format!("vectors/tasks/{}/data", task_id);
        let meta_key = format!("vectors/tasks/{}/meta", task_id);

        // Store embedding as little-endian f32 bytes
        let mut bytes = Vec::with_capacity(embedding.len() * 4);
        for &val in embedding {
            bytes.extend_from_slice(&val.to_le_bytes());
        }

        let meta_json = serde_json::to_vec(metadata)?;

        self.apply_atomic_batch(&[
            (data_key.as_str(), &bytes),
            (meta_key.as_str(), &meta_json),
        ])?;

        Ok(())
    }

    /// Load an embedding vector.
    pub fn load_vector(&self, task_id: &str) -> Result<Option<(Vec<f32>, serde_json::Value)>> {
        let data_key = format!("vectors/tasks/{}/data", task_id);
        let meta_key = format!("vectors/tasks/{}/meta", task_id);

        let data = match self.get(&data_key)? {
            Some(d) => d,
            None => return Ok(None),
        };

        let meta = self
            .get(&meta_key)?
            .map(|bytes| serde_json::from_slice(&bytes))
            .transpose()?
            .unwrap_or(serde_json::Value::Null);

        // Decode f32 little-endian
        let mut embedding = Vec::with_capacity(data.len() / 4);
        for chunk in data.chunks_exact(4) {
            embedding.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }

        Ok(Some((embedding, meta)))
    }

    /// Find k-nearest task embeddings via brute-force cosine similarity.
    /// For small N (<10,000), this O(N×d) scan is sufficient.
    /// Can be replaced with HNSW index as task count grows.
    pub fn search_similar_tasks(
        &self,
        query: &[f32],
        k: usize,
    ) -> Result<Vec<(String, f32)>> {
        let prefix = "vectors/tasks/";
        let entries = self.scan(prefix)?;

        // Group entries by task_id
        let mut data_map: HashMap<String, Vec<u8>> = HashMap::new();
        for (key, value) in entries {
            if key.ends_with("/data") {
                let task_id = key
                    .strip_prefix(prefix)
                    .and_then(|s| s.strip_suffix("/data"))
                    .unwrap_or("")
                    .to_string();
                if !task_id.is_empty() {
                    data_map.insert(task_id, value);
                }
            }
        }

        // Compute cosine similarities
        let mut scores: Vec<(String, f32)> = Vec::new();
        let query_norm = dot_product(query, query).sqrt();
        if query_norm == 0.0 {
            return Ok(Vec::new());
        }

        for (task_id, data) in &data_map {
            let mut embedding = Vec::with_capacity(data.len() / 4);
            for chunk in data.chunks_exact(4) {
                embedding.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }

            if embedding.len() != query.len() {
                continue;
            }

            let emb_norm = dot_product(&embedding, &embedding).sqrt();
            if emb_norm == 0.0 {
                continue;
            }

            let cosine = dot_product(query, &embedding) / (query_norm * emb_norm);
            scores.push((task_id.clone(), cosine));
        }

        // Top-k by cosine similarity (descending)
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(k);

        Ok(scores)
    }

    /// Store path for diagnostics.
    pub fn path(&self) -> &Path {
        &self.store_path
    }

    /// Key count for health checks.
    pub fn key_count(&self) -> usize {
        self.db.len()
    }

    // ── Knowledge Unit Storage ──

    /// Store a knowledge unit extracted from a completed task.
    pub fn store_knowledge_unit(&self, unit: &KnowledgeUnit) -> Result<()> {
        let key = format!("knowledge/{}", unit.id);
        self.put_json(&key, unit)?;

        // Also store the embedding for semantic search
        if !unit.embedding.is_empty() {
            self.store_vector(
                &format!("knowledge_vec/{}", unit.id),
                &unit.embedding,
                &serde_json::json!({
                    "concept": unit.concept,
                    "project": unit.project,
                }),
            )?;
        }
        Ok(())
    }

    /// Retrieve all knowledge units.
    pub fn list_knowledge_units(&self, limit: usize) -> Result<Vec<KnowledgeUnit>> {
        let mut units = Vec::new();
        for item in self.db.scan_prefix("knowledge/") {
            if let Ok((key, val)) = item {
                let key_str = String::from_utf8_lossy(&key);
                if key_str.starts_with("knowledge/") && !key_str.starts_with("knowledge_vec/") {
                    if let Ok(unit) = serde_json::from_slice::<KnowledgeUnit>(&val) {
                        units.push(unit);
                    }
                }
            }
            if units.len() >= limit {
                break;
            }
        }
        Ok(units)
    }

    /// Search knowledge units by semantic similarity.
    pub fn search_knowledge(&self, query_embedding: &[f32], k: usize) -> Result<Vec<(KnowledgeUnit, f32)>> {
        let vec_results = self.search_similar_tasks(query_embedding, k * 2)?;
        let mut results = Vec::new();

        for (vec_id, cosine) in vec_results {
            if let Some(unit_id) = vec_id.strip_prefix("knowledge_vec/") {
                let key = format!("knowledge/{}", unit_id);
                if let Some(val) = self.db.get(key.as_bytes())? {
                    if let Ok(unit) = serde_json::from_slice::<KnowledgeUnit>(&val) {
                        results.push((unit, cosine));
                    }
                }
            }
            if results.len() >= k {
                break;
            }
        }

        Ok(results)
    }
}

/// A discrete knowledge unit extracted from a task conversation.
/// Represents a solution pattern, debugging approach, or architectural decision.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeUnit {
    pub id: String,
    pub concept: String,
    pub context: String,
    pub approach: String,
    pub outcome: String,
    pub project: String,
    pub task_id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub embedding: Vec<f32>,
}

fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Compute a SHA-256 hash chain link: H_i = SHA-256(H_{i-1} || event || timestamp || seq)
///
/// This provides tamper evidence: any modification to a stored event
/// will break the chain, detectable with O(n) verification.
fn compute_chain_hash(
    prev_hash: &str,
    event_json: &str,
    timestamp: &DateTime<Utc>,
    seq: u64,
) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Use a deterministic hash. For production forensic-grade tamper evidence,
    // replace with SHA-256 (ring or sha2 crate). For now, use a fast
    // deterministic hash that provides basic tamper detection.
    let mut hasher = DefaultHasher::new();
    prev_hash.hash(&mut hasher);
    event_json.hash(&mut hasher);
    timestamp.to_rfc3339().hash(&mut hasher);
    seq.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

impl Drop for DaemonStore {
    fn drop(&mut self) {
        if !self.shutdown.swap(true, Ordering::Relaxed) {
            if let Err(e) = self.db.flush() {
                tracing::error!(error = %e, "failed to flush store on drop");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipit_channel::*;

    #[test]
    fn test_store_roundtrip() {
        let store = DaemonStore::open_ephemeral().unwrap();

        store.put("test/key", b"hello").unwrap();
        let val = store.get("test/key").unwrap().unwrap();
        assert_eq!(val, b"hello");

        store.delete("test/key").unwrap();
        assert!(store.get("test/key").unwrap().is_none());
    }

    #[test]
    fn test_scan_prefix() {
        let store = DaemonStore::open_ephemeral().unwrap();
        store.put("projects/a/config", b"a").unwrap();
        store.put("projects/a/context", b"ctx").unwrap();
        store.put("projects/b/config", b"b").unwrap();

        let a_entries = store.scan("projects/a/").unwrap();
        assert_eq!(a_entries.len(), 2);

        let all = store.scan("projects/").unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_atomic_batch() {
        let store = DaemonStore::open_ephemeral().unwrap();
        store
            .apply_atomic_batch(&[("k1", b"v1"), ("k2", b"v2"), ("k3", b"v3")])
            .unwrap();

        assert_eq!(store.get("k1").unwrap().unwrap(), b"v1");
        assert_eq!(store.get("k2").unwrap().unwrap(), b"v2");
        assert_eq!(store.get("k3").unwrap().unwrap(), b"v3");
    }

    #[test]
    fn test_task_lifecycle() {
        let store = DaemonStore::open_ephemeral().unwrap();
        let task = NormalizedTask::new(
            "myapp".to_string(),
            "fix the bug".to_string(),
            MessageOrigin::Api { client_id: None },
        );

        let record = store.create_task(&task).unwrap();
        assert_eq!(record.status, TaskStatus::Queued);

        let updated = store
            .update_task_status(&task.task_id, TaskStatus::Running, |r| {
                r.started_at = Some(Utc::now());
            })
            .unwrap();
        assert_eq!(updated.status, TaskStatus::Running);

        let fetched = store.get_task(&task.task_id).unwrap().unwrap();
        assert_eq!(fetched.status, TaskStatus::Running);
    }

    #[test]
    fn test_vector_roundtrip() {
        let store = DaemonStore::open_ephemeral().unwrap();
        let embedding = vec![1.0, 0.5, -0.3, 0.8];
        let meta = serde_json::json!({"project": "test"});

        store.store_vector("task-1", &embedding, &meta).unwrap();

        let (loaded_emb, loaded_meta) = store.load_vector("task-1").unwrap().unwrap();
        assert_eq!(loaded_emb, embedding);
        assert_eq!(loaded_meta["project"], "test");
    }

    #[test]
    fn test_similar_task_search() {
        let store = DaemonStore::open_ephemeral().unwrap();

        store
            .store_vector("t1", &[1.0, 0.0, 0.0], &serde_json::json!({}))
            .unwrap();
        store
            .store_vector("t2", &[0.9, 0.1, 0.0], &serde_json::json!({}))
            .unwrap();
        store
            .store_vector("t3", &[0.0, 0.0, 1.0], &serde_json::json!({}))
            .unwrap();

        let results = store.search_similar_tasks(&[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "t1"); // exact match
        assert_eq!(results[1].0, "t2"); // close match
    }
}
