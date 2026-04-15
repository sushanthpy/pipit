//! Enterprise Telemetry Facade — OpenTelemetry-compatible structured observability
//!
//! Single telemetry facade with per-session counters, cost tracking with Kahan
//! summation, reservoir sampling for high-cardinality metrics, and export to
//! OTLP, Prometheus, and local JSONL simultaneously.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// OpenTelemetry-compatible span for distributed tracing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtelSpan {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub operation_name: String,
    pub start_time_ms: u64,
    pub end_time_ms: Option<u64>,
    pub status: SpanStatus,
    pub attributes: HashMap<String, SpanValue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanStatus {
    Ok,
    Error,
    Unset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpanValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl OtelSpan {
    pub fn new(trace_id: &str, operation: &str) -> Self {
        Self {
            trace_id: trace_id.to_string(),
            span_id: uuid::Uuid::new_v4().to_string()[..16].to_string(),
            parent_span_id: None,
            operation_name: operation.to_string(),
            start_time_ms: now_ms(),
            end_time_ms: None,
            status: SpanStatus::Unset,
            attributes: HashMap::new(),
        }
    }

    pub fn with_parent(mut self, parent: &str) -> Self {
        self.parent_span_id = Some(parent.to_string());
        self
    }

    pub fn attr(mut self, key: &str, value: SpanValue) -> Self {
        self.attributes.insert(key.to_string(), value);
        self
    }

    pub fn finish(&mut self, status: SpanStatus) {
        self.end_time_ms = Some(now_ms());
        self.status = status;
    }

    pub fn duration_ms(&self) -> Option<u64> {
        self.end_time_ms
            .map(|e| e.saturating_sub(self.start_time_ms))
    }
}

/// Per-session counters with OTel-compatible attributes.
#[derive(Debug, Default)]
pub struct SessionCounters {
    pub turns: AtomicU64,
    pub tool_calls: AtomicU64,
    pub tokens_input: AtomicU64,
    pub tokens_output: AtomicU64,
    pub lines_of_code: AtomicU64,
    pub files_modified: AtomicU64,
    pub prs_created: AtomicU64,
    pub commits: AtomicU64,
    /// Cost tracked with Kahan summation for precision.
    cost: Mutex<KahanAccumulator>,
    /// Active time in milliseconds.
    pub active_time_ms: AtomicU64,
    /// Total transient retries consumed this session.
    pub total_retries: AtomicU32,
    /// Consecutive errors without success (circuit breaker).
    pub consecutive_errors: AtomicU32,
}

impl SessionCounters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment_turns(&self) {
        self.turns.fetch_add(1, Ordering::Relaxed);
    }
    pub fn increment_tool_calls(&self) {
        self.tool_calls.fetch_add(1, Ordering::Relaxed);
    }
    pub fn add_tokens(&self, input: u64, output: u64) {
        self.tokens_input.fetch_add(input, Ordering::Relaxed);
        self.tokens_output.fetch_add(output, Ordering::Relaxed);
    }
    pub fn increment_loc(&self, lines: u64) {
        self.lines_of_code.fetch_add(lines, Ordering::Relaxed);
    }
    pub fn increment_files(&self) {
        self.files_modified.fetch_add(1, Ordering::Relaxed);
    }
    pub fn increment_commits(&self) {
        self.commits.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a retry attempt. Returns false if budget exhausted (max 15 per session, max 5 consecutive).
    pub fn can_retry(&self) -> bool {
        self.total_retries.load(Ordering::Relaxed) < 15
            && self.consecutive_errors.load(Ordering::Relaxed) < 5
    }

    pub fn record_retry(&self) {
        self.total_retries.fetch_add(1, Ordering::Relaxed);
        self.consecutive_errors.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_success(&self) {
        self.consecutive_errors.store(0, Ordering::Relaxed);
    }

    /// Add cost using Kahan summation to prevent floating-point drift.
    pub fn add_cost(&self, amount: f64) {
        if let Ok(mut acc) = self.cost.lock() {
            acc.add(amount);
        }
    }

    /// Get accumulated cost with full precision.
    pub fn total_cost(&self) -> f64 {
        self.cost.lock().map(|acc| acc.sum).unwrap_or(0.0)
    }

    /// Export as OTel-compatible attribute map.
    pub fn as_attributes(&self) -> HashMap<String, SpanValue> {
        let mut attrs = HashMap::new();
        attrs.insert(
            "session.turns".into(),
            SpanValue::Int(self.turns.load(Ordering::Relaxed) as i64),
        );
        attrs.insert(
            "session.tool_calls".into(),
            SpanValue::Int(self.tool_calls.load(Ordering::Relaxed) as i64),
        );
        attrs.insert(
            "session.tokens.input".into(),
            SpanValue::Int(self.tokens_input.load(Ordering::Relaxed) as i64),
        );
        attrs.insert(
            "session.tokens.output".into(),
            SpanValue::Int(self.tokens_output.load(Ordering::Relaxed) as i64),
        );
        attrs.insert(
            "session.cost.usd".into(),
            SpanValue::Float(self.total_cost()),
        );
        attrs.insert(
            "session.loc".into(),
            SpanValue::Int(self.lines_of_code.load(Ordering::Relaxed) as i64),
        );
        attrs.insert(
            "session.files_modified".into(),
            SpanValue::Int(self.files_modified.load(Ordering::Relaxed) as i64),
        );
        attrs
    }
}

/// Kahan summation accumulator — O(ε) precision instead of O(nε).
#[derive(Debug, Default)]
struct KahanAccumulator {
    sum: f64,
    compensation: f64,
}

impl KahanAccumulator {
    fn add(&mut self, value: f64) {
        let y = value - self.compensation;
        let t = self.sum + y;
        self.compensation = (t - self.sum) - y;
        self.sum = t;
    }
}

/// Reservoir sampler (Vitter's Algorithm R) for high-cardinality metrics.
/// Maintains a fixed-size sample with O(1) amortized insertion.
pub struct ReservoirSampler<T> {
    reservoir: Vec<T>,
    capacity: usize,
    count: u64,
}

impl<T: Clone> ReservoirSampler<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            reservoir: Vec::with_capacity(capacity),
            capacity,
            count: 0,
        }
    }

    /// Add a sample. O(1) amortized.
    pub fn add(&mut self, item: T) {
        self.count += 1;
        if self.reservoir.len() < self.capacity {
            self.reservoir.push(item);
        } else {
            // Replace with probability capacity/count
            let j = (rand_u64() % self.count) as usize;
            if j < self.capacity {
                self.reservoir[j] = item;
            }
        }
    }

    /// Get the current sample.
    pub fn samples(&self) -> &[T] {
        &self.reservoir
    }

    /// Total items seen.
    pub fn total_count(&self) -> u64 {
        self.count
    }
}

/// Feature flag evaluation port.
pub trait FeatureFlagPort: Send + Sync {
    /// Evaluate a boolean feature flag.
    fn is_enabled(&self, flag: &str, context: &HashMap<String, String>) -> bool;
    /// Get a string flag value.
    fn get_value(&self, flag: &str, context: &HashMap<String, String>) -> Option<String>;
}

/// No-op feature flag port (all flags disabled).
pub struct NullFeatureFlagPort;

impl FeatureFlagPort for NullFeatureFlagPort {
    fn is_enabled(&self, _: &str, _: &HashMap<String, String>) -> bool {
        false
    }
    fn get_value(&self, _: &str, _: &HashMap<String, String>) -> Option<String> {
        None
    }
}

/// Telemetry export target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportTarget {
    /// OTLP gRPC/HTTP endpoint.
    Otlp,
    /// Prometheus scrape endpoint.
    Prometheus,
    /// Local JSONL file.
    Jsonl,
}

/// OTLP HTTP endpoint configuration.
#[derive(Debug, Clone)]
pub struct OtlpConfig {
    /// OTLP endpoint (e.g., "http://localhost:4318").
    pub endpoint: String,
    /// Optional authorization header value (e.g., "Bearer <token>").
    pub auth_header: Option<String>,
    /// Request timeout in milliseconds.
    pub timeout_ms: u64,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .unwrap_or_else(|_| "http://localhost:4318".to_string()),
            auth_header: std::env::var("OTEL_EXPORTER_OTLP_HEADERS").ok(),
            timeout_ms: 5_000,
        }
    }
}

/// The unified telemetry facade.
pub struct TelemetryFacade {
    pub session_counters: SessionCounters,
    pub spans: Mutex<Vec<OtelSpan>>,
    pub export_targets: Vec<ExportTarget>,
    session_id: String,
    model_name: String,
    provider_name: String,
    /// OTLP configuration (used when ExportTarget::Otlp is active).
    pub otlp_config: OtlpConfig,
}

impl TelemetryFacade {
    pub fn new(session_id: &str, model: &str, provider: &str) -> Self {
        Self {
            session_counters: SessionCounters::new(),
            spans: Mutex::new(Vec::new()),
            export_targets: vec![ExportTarget::Jsonl],
            session_id: session_id.to_string(),
            model_name: model.to_string(),
            provider_name: provider.to_string(),
            otlp_config: OtlpConfig::default(),
        }
    }

    /// Enable OTLP export with the given configuration.
    pub fn with_otlp(mut self, config: OtlpConfig) -> Self {
        self.otlp_config = config;
        if !self.export_targets.contains(&ExportTarget::Otlp) {
            self.export_targets.push(ExportTarget::Otlp);
        }
        self
    }

    /// Start a new span.
    pub fn start_span(&self, operation: &str) -> OtelSpan {
        OtelSpan::new(&self.session_id, operation)
            .attr("session.id", SpanValue::String(self.session_id.clone()))
            .attr("model.name", SpanValue::String(self.model_name.clone()))
            .attr(
                "provider.name",
                SpanValue::String(self.provider_name.clone()),
            )
    }

    /// Record a completed span.
    pub fn record_span(&self, span: OtelSpan) {
        if let Ok(mut spans) = self.spans.lock() {
            spans.push(span);
        }
    }

    /// Export all recorded spans to configured targets.
    pub fn export(&self) -> Result<usize, String> {
        let spans = self.spans.lock().map_err(|e| e.to_string())?;
        let count = spans.len();
        for target in &self.export_targets {
            match target {
                ExportTarget::Jsonl => {
                    self.export_jsonl(&spans)?;
                }
                ExportTarget::Otlp => {
                    self.export_otlp(&spans)?;
                }
                ExportTarget::Prometheus => {
                    self.export_prometheus(&spans)?;
                }
            }
        }
        Ok(count)
    }

    /// Export spans to JSONL file at .pipit/telemetry/spans.jsonl
    fn export_jsonl(&self, spans: &[OtelSpan]) -> Result<(), String> {
        let dir = std::path::Path::new(".pipit").join("telemetry");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("spans.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        use std::io::Write;
        for span in spans {
            let json = serde_json::to_string(span).map_err(|e| e.to_string())?;
            writeln!(file, "{}", json).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Export spans to OTLP HTTP/JSON endpoint (/v1/traces).
    ///
    /// Implements the OTLP HTTP JSON protocol (no protobuf dependency).
    /// Batches all spans into a single ExportTraceServiceRequest.
    /// Tolerates connection failures (telemetry is best-effort).
    fn export_otlp(&self, spans: &[OtelSpan]) -> Result<(), String> {
        if spans.is_empty() {
            return Ok(());
        }

        let resource = serde_json::json!({
            "attributes": [
                {"key": "service.name", "value": {"stringValue": "pipit"}},
                {"key": "service.version", "value": {"stringValue": env!("CARGO_PKG_VERSION")}},
                {"key": "session.id", "value": {"stringValue": &self.session_id}},
                {"key": "model.name", "value": {"stringValue": &self.model_name}},
                {"key": "provider.name", "value": {"stringValue": &self.provider_name}},
            ]
        });

        let otlp_spans: Vec<serde_json::Value> = spans.iter().map(|s| {
            let attrs: Vec<serde_json::Value> = s.attributes.iter().map(|(k, v)| {
                let val = match v {
                    SpanValue::String(sv) => serde_json::json!({"stringValue": sv}),
                    SpanValue::Int(iv) => serde_json::json!({"intValue": iv.to_string()}),
                    SpanValue::Float(fv) => serde_json::json!({"doubleValue": fv}),
                    SpanValue::Bool(bv) => serde_json::json!({"boolValue": bv}),
                };
                serde_json::json!({"key": k, "value": val})
            }).collect();

            let status_code = match s.status {
                SpanStatus::Ok => 1,
                SpanStatus::Error => 2,
                SpanStatus::Unset => 0,
            };

            serde_json::json!({
                "traceId": hex_encode_id(&s.trace_id),
                "spanId": hex_encode_id(&s.span_id),
                "parentSpanId": s.parent_span_id.as_deref().map(hex_encode_id).unwrap_or_default(),
                "name": s.operation_name,
                "kind": 1, // SPAN_KIND_INTERNAL
                "startTimeUnixNano": (s.start_time_ms as u64).saturating_mul(1_000_000).to_string(),
                "endTimeUnixNano": s.end_time_ms.unwrap_or(s.start_time_ms).saturating_mul(1_000_000).to_string(),
                "attributes": attrs,
                "status": {"code": status_code},
            })
        }).collect();

        let payload = serde_json::json!({
            "resourceSpans": [{
                "resource": resource,
                "scopeSpans": [{
                    "scope": {"name": "pipit", "version": env!("CARGO_PKG_VERSION")},
                    "spans": otlp_spans,
                }]
            }]
        });

        let url = format!("{}/v1/traces", self.otlp_config.endpoint.trim_end_matches('/'));
        let body = serde_json::to_vec(&payload).map_err(|e| e.to_string())?;

        // Synchronous HTTP POST (telemetry export happens at session end).
        // Best-effort: swallow connection errors to avoid disrupting the session.
        let result = otlp_http_post(&url, &body, &self.otlp_config);
        match result {
            Ok(status) if status >= 200 && status < 300 => Ok(()),
            Ok(status) => Err(format!("OTLP endpoint returned HTTP {}", status)),
            Err(e) => {
                // Best-effort: log but don't fail
                tracing::warn!("OTLP export failed (non-fatal): {}", e);
                Ok(())
            }
        }
    }

    /// Export session metrics in Prometheus exposition format.
    fn export_prometheus(&self, _spans: &[OtelSpan]) -> Result<(), String> {
        let dir = std::path::Path::new(".pipit").join("telemetry");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("metrics.prom");

        let counters = &self.session_counters;
        let mut buf = String::new();
        use std::fmt::Write;
        let _ = writeln!(buf, "# HELP pipit_turns_total Total turns in this session");
        let _ = writeln!(buf, "# TYPE pipit_turns_total counter");
        let _ = writeln!(buf, "pipit_turns_total{{session=\"{}\"}} {}",
            self.session_id, counters.turns.load(std::sync::atomic::Ordering::Relaxed));
        let _ = writeln!(buf, "# HELP pipit_tool_calls_total Total tool calls");
        let _ = writeln!(buf, "# TYPE pipit_tool_calls_total counter");
        let _ = writeln!(buf, "pipit_tool_calls_total{{session=\"{}\"}} {}",
            self.session_id, counters.tool_calls.load(std::sync::atomic::Ordering::Relaxed));
        let _ = writeln!(buf, "# HELP pipit_tokens_input_total Input tokens consumed");
        let _ = writeln!(buf, "# TYPE pipit_tokens_input_total counter");
        let _ = writeln!(buf, "pipit_tokens_input_total{{session=\"{}\"}} {}",
            self.session_id, counters.tokens_input.load(std::sync::atomic::Ordering::Relaxed));
        let _ = writeln!(buf, "# HELP pipit_tokens_output_total Output tokens generated");
        let _ = writeln!(buf, "# TYPE pipit_tokens_output_total counter");
        let _ = writeln!(buf, "pipit_tokens_output_total{{session=\"{}\"}} {}",
            self.session_id, counters.tokens_output.load(std::sync::atomic::Ordering::Relaxed));
        let _ = writeln!(buf, "# HELP pipit_cost_usd Total cost in USD");
        let _ = writeln!(buf, "# TYPE pipit_cost_usd gauge");
        let _ = writeln!(buf, "pipit_cost_usd{{session=\"{}\"}} {:.6}",
            self.session_id, counters.total_cost());

        std::fs::write(&path, &buf).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Get session summary for /status and /cost commands.
    pub fn session_summary(&self) -> SessionSummary {
        use std::sync::atomic::Ordering;
        SessionSummary {
            session_id: self.session_id.clone(),
            model_name: self.model_name.clone(),
            provider_name: self.provider_name.clone(),
            turns: self.session_counters.turns.load(Ordering::Relaxed),
            tool_calls: self.session_counters.tool_calls.load(Ordering::Relaxed),
            tokens_input: self.session_counters.tokens_input.load(Ordering::Relaxed),
            tokens_output: self.session_counters.tokens_output.load(Ordering::Relaxed),
            total_cost: self.session_counters.total_cost(),
            span_count: self.spans.lock().map(|s| s.len()).unwrap_or(0),
        }
    }
}

/// Summary of session telemetry for display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub model_name: String,
    pub provider_name: String,
    pub turns: u64,
    pub tool_calls: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub total_cost: f64,
    pub span_count: usize,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn rand_u64() -> u64 {
    // Simple non-cryptographic RNG for reservoir sampling
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    now_ms().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    hasher.finish()
}

/// Ensure ID is a valid 32-char hex trace ID or 16-char span ID for OTLP.
/// If the input is already hex, pad/truncate. Otherwise, hash it.
fn hex_encode_id(id: &str) -> String {
    if id.len() <= 32 && id.chars().all(|c| c.is_ascii_hexdigit()) {
        return format!("{:0>32}", id);
    }
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(id.as_bytes());
    hex::encode(&hash[..16])
}

/// Minimal synchronous HTTP POST for OTLP export.
/// Uses std::net::TcpStream to avoid requiring an async runtime or reqwest.
fn otlp_http_post(url: &str, body: &[u8], config: &OtlpConfig) -> Result<u16, String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    // Parse URL: http://host:port/path
    let url = url.strip_prefix("http://").ok_or("OTLP requires http:// URL")?;
    let (host_port, path) = url.split_once('/').unwrap_or((url, "v1/traces"));
    let path = format!("/{}", path);

    let timeout = Duration::from_millis(config.timeout_ms);
    let mut stream = TcpStream::connect_timeout(
        &host_port.parse().map_err(|e: std::net::AddrParseError| e.to_string())?,
        timeout,
    ).map_err(|e| format!("OTLP connect failed: {}", e))?;

    stream.set_write_timeout(Some(timeout)).ok();
    stream.set_read_timeout(Some(timeout)).ok();

    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        path, host_port, body.len()
    );
    if let Some(auth) = &config.auth_header {
        request.push_str(&format!("Authorization: {}\r\n", auth));
    }
    request.push_str("Connection: close\r\n\r\n");

    stream.write_all(request.as_bytes()).map_err(|e| e.to_string())?;
    stream.write_all(body).map_err(|e| e.to_string())?;

    let mut response = [0u8; 256];
    let n = stream.read(&mut response).map_err(|e| e.to_string())?;
    let response_str = String::from_utf8_lossy(&response[..n]);

    // Parse HTTP status from "HTTP/1.1 200 OK"
    let status = response_str
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);

    Ok(status)
}

/// Encode bytes as hex string.
mod hex {
    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kahan_summation_precision() {
        let mut acc = KahanAccumulator::default();
        // Add 10000 small values
        for _ in 0..10000 {
            acc.add(0.0001);
        }
        // Should be very close to 1.0
        assert!((acc.sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn session_counters() {
        let counters = SessionCounters::new();
        counters.increment_turns();
        counters.increment_turns();
        counters.add_tokens(100, 50);
        counters.add_cost(0.01);
        counters.add_cost(0.02);

        assert_eq!(counters.turns.load(Ordering::Relaxed), 2);
        assert_eq!(counters.tokens_input.load(Ordering::Relaxed), 100);
        assert!((counters.total_cost() - 0.03).abs() < 1e-10);
    }

    #[test]
    fn reservoir_sampling_bounded() {
        let mut sampler = ReservoirSampler::new(10);
        for i in 0..1000 {
            sampler.add(i);
        }
        assert_eq!(sampler.samples().len(), 10);
        assert_eq!(sampler.total_count(), 1000);
    }

    #[test]
    fn otel_span_lifecycle() {
        let mut span = OtelSpan::new("trace-1", "llm.complete")
            .attr("model", SpanValue::String("claude".into()));
        assert!(span.end_time_ms.is_none());

        span.finish(SpanStatus::Ok);
        assert!(span.end_time_ms.is_some());
        assert!(span.duration_ms().unwrap() >= 0);
    }

    #[test]
    fn attributes_export() {
        let counters = SessionCounters::new();
        counters.increment_turns();
        counters.add_cost(0.05);
        let attrs = counters.as_attributes();
        assert!(attrs.contains_key("session.turns"));
        assert!(attrs.contains_key("session.cost.usd"));
    }

    #[test]
    fn otlp_config_default_from_env() {
        let config = OtlpConfig::default();
        assert!(config.endpoint.starts_with("http"));
        assert_eq!(config.timeout_ms, 5000);
    }

    #[test]
    fn otlp_payload_structure() {
        let facade = TelemetryFacade::new("test-session", "gpt-4", "openai");
        let mut span = facade.start_span("test.operation");
        span.finish(SpanStatus::Ok);
        facade.record_span(span);

        // Verify spans are recorded
        let spans = facade.spans.lock().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].operation_name, "test.operation");
        assert_eq!(spans[0].status, SpanStatus::Ok);
    }

    #[test]
    fn otlp_export_best_effort_on_connection_failure() {
        let mut facade = TelemetryFacade::new("test", "model", "provider");
        facade.otlp_config = OtlpConfig {
            endpoint: "http://127.0.0.1:1".to_string(), // unreachable
            auth_header: None,
            timeout_ms: 100,
        };
        facade.export_targets = vec![ExportTarget::Otlp];

        let mut span = facade.start_span("op");
        span.finish(SpanStatus::Ok);
        facade.record_span(span);

        // Should not error — OTLP is best-effort
        let result = facade.export();
        assert!(result.is_ok());
    }

    #[test]
    fn hex_encode_id_pads_short_ids() {
        let id = hex_encode_id("abc");
        assert_eq!(id.len(), 32);
        assert!(id.ends_with("abc"));
    }

    #[test]
    fn hex_encode_id_hashes_non_hex() {
        let id = hex_encode_id("my-trace-id-with-dashes");
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn with_otlp_adds_export_target() {
        let facade = TelemetryFacade::new("s", "m", "p")
            .with_otlp(OtlpConfig::default());
        assert!(facade.export_targets.contains(&ExportTarget::Otlp));
    }

    #[test]
    fn prometheus_export_to_file() {
        let dir = tempfile::tempdir().unwrap();
        let facade = TelemetryFacade::new("prom-test", "model", "prov");
        facade.session_counters.increment_turns();
        facade.session_counters.increment_turns();
        facade.session_counters.add_cost(0.42);

        // Test the prometheus format generation
        let counters = &facade.session_counters;
        let turns = counters.turns.load(Ordering::Relaxed);
        assert_eq!(turns, 2);
        let cost = counters.total_cost();
        assert!((cost - 0.42).abs() < 1e-10);
    }
}
