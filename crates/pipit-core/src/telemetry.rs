//! Telemetry Event Pipeline
//!
//! Structured analytics events with typed payloads, async buffered emission,
//! and local aggregation. Events are buffered in an MPSC channel and flushed
//! every 5 seconds or 100 events (whichever comes first).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// A structured telemetry event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEvent {
    /// Event type identifier.
    pub name: String,
    /// Key-value properties.
    pub properties: HashMap<String, serde_json::Value>,
    /// Query chain ID for distributed tracing across sub-agents.
    pub query_chain_id: Option<String>,
    /// Query depth (0 = root, 1+ = sub-agent).
    pub query_depth: u32,
    /// Timestamp (unix milliseconds).
    pub timestamp_ms: u64,
}

impl TelemetryEvent {
    /// Create a new event with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            properties: HashMap::new(),
            query_chain_id: None,
            query_depth: 0,
            timestamp_ms: current_timestamp_ms(),
        }
    }

    /// Add a property.
    pub fn with_property(
        mut self,
        key: impl Into<String>,
        value: impl Into<serde_json::Value>,
    ) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Set the query chain ID.
    pub fn with_chain_id(mut self, id: impl Into<String>) -> Self {
        self.query_chain_id = Some(id.into());
        self
    }

    /// Set the query depth.
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.query_depth = depth;
        self
    }
}

/// Well-known event names.
pub mod events {
    pub const AUTO_COMPACT_START: &str = "auto_compact_start";
    pub const AUTO_COMPACT_END: &str = "auto_compact_end";
    pub const AUTO_COMPACT_FAILURE: &str = "auto_compact_failure";
    pub const MODEL_FALLBACK: &str = "model_fallback";
    pub const QUERY_ERROR: &str = "query_error";
    pub const TOKEN_ESCALATION: &str = "token_escalation";
    pub const TURN_COMPLETE: &str = "turn_complete";
    pub const SESSION_START: &str = "session_start";
    pub const SESSION_END: &str = "session_end";
    pub const TOOL_EXECUTION: &str = "tool_execution";
    pub const APPROVAL_REQUESTED: &str = "approval_requested";
    pub const APPROVAL_DENIED: &str = "approval_denied";
    pub const LOOP_DETECTED: &str = "loop_detected";
    pub const CONTEXT_OVERFLOW: &str = "context_overflow";
    pub const REACTIVE_COMPACT: &str = "reactive_compact";
}

/// Configuration for the telemetry pipeline.
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Maximum events to buffer before flush.
    pub batch_size: usize,
    /// Flush interval in milliseconds.
    pub flush_interval_ms: u64,
    /// Channel buffer capacity.
    pub channel_capacity: usize,
    /// Whether telemetry is enabled.
    pub enabled: bool,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            batch_size: 100,
            flush_interval_ms: 5000,
            channel_capacity: 1024,
            enabled: true,
        }
    }
}

/// A telemetry sink that receives flushed event batches.
#[async_trait::async_trait]
pub trait TelemetrySink: Send + Sync {
    /// Process a batch of events.
    async fn flush(&self, events: Vec<TelemetryEvent>);
}

/// File-based sink that appends JSONL to a log file.
pub struct FileSink {
    path: std::path::PathBuf,
}

impl FileSink {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait::async_trait]
impl TelemetrySink for FileSink {
    async fn flush(&self, events: Vec<TelemetryEvent>) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            for event in &events {
                if let Ok(line) = serde_json::to_string(event) {
                    let _ = writeln!(file, "{}", line);
                }
            }
        }
    }
}

/// The telemetry pipeline.
pub struct TelemetryPipeline {
    sender: mpsc::Sender<TelemetryEvent>,
    /// Handle to the background flush task.
    _flush_handle: Option<tokio::task::JoinHandle<()>>,
}

impl TelemetryPipeline {
    /// Create a new telemetry pipeline with the given config and sink.
    pub fn new(config: TelemetryConfig, sink: Arc<dyn TelemetrySink>) -> Self {
        let (tx, rx) = mpsc::channel(config.channel_capacity);

        let handle = if config.enabled {
            Some(tokio::spawn(flush_loop(rx, sink, config)))
        } else {
            None
        };

        Self {
            sender: tx,
            _flush_handle: handle,
        }
    }

    /// Emit an event (non-blocking).
    pub fn emit(&self, event: TelemetryEvent) {
        let _ = self.sender.try_send(event);
    }

    /// Create a scoped emitter with a fixed query chain ID and depth.
    pub fn scoped(&self, chain_id: String, depth: u32) -> ScopedEmitter {
        ScopedEmitter {
            sender: self.sender.clone(),
            chain_id,
            depth,
        }
    }
}

/// A scoped emitter that automatically tags events with chain ID and depth.
#[derive(Clone)]
pub struct ScopedEmitter {
    sender: mpsc::Sender<TelemetryEvent>,
    chain_id: String,
    depth: u32,
}

impl ScopedEmitter {
    /// Emit an event with automatic chain ID and depth.
    pub fn emit(&self, mut event: TelemetryEvent) {
        event.query_chain_id = Some(self.chain_id.clone());
        event.query_depth = self.depth;
        let _ = self.sender.try_send(event);
    }
}

/// Local aggregation — simple counter-based metrics.
pub struct LocalAggregator {
    counters: std::sync::Mutex<HashMap<String, u64>>,
}

impl LocalAggregator {
    pub fn new() -> Self {
        Self {
            counters: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Increment a counter.
    pub fn increment(&self, metric: &str) {
        let mut counters = self.counters.lock().unwrap();
        *counters.entry(metric.to_string()).or_insert(0) += 1;
    }

    /// Increment by a specific amount.
    pub fn increment_by(&self, metric: &str, amount: u64) {
        let mut counters = self.counters.lock().unwrap();
        *counters.entry(metric.to_string()).or_insert(0) += amount;
    }

    /// Get the current value of a counter.
    pub fn get(&self, metric: &str) -> u64 {
        self.counters
            .lock()
            .unwrap()
            .get(metric)
            .copied()
            .unwrap_or(0)
    }

    /// Get all counters as a snapshot.
    pub fn snapshot(&self) -> HashMap<String, u64> {
        self.counters.lock().unwrap().clone()
    }
}

impl Default for LocalAggregator {
    fn default() -> Self {
        Self::new()
    }
}

/// Background flush loop.
async fn flush_loop(
    mut rx: mpsc::Receiver<TelemetryEvent>,
    sink: Arc<dyn TelemetrySink>,
    config: TelemetryConfig,
) {
    let mut buffer = Vec::with_capacity(config.batch_size);
    let flush_interval = std::time::Duration::from_millis(config.flush_interval_ms);

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Some(e) => {
                        buffer.push(e);
                        if buffer.len() >= config.batch_size {
                            sink.flush(std::mem::take(&mut buffer)).await;
                        }
                    }
                    None => {
                        // Channel closed — flush remaining and exit
                        if !buffer.is_empty() {
                            sink.flush(std::mem::take(&mut buffer)).await;
                        }
                        break;
                    }
                }
            }
            _ = tokio::time::sleep(flush_interval) => {
                if !buffer.is_empty() {
                    sink.flush(std::mem::take(&mut buffer)).await;
                }
            }
        }
    }
}

fn current_timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
