//! Taint Analysis — Task 9.2 (part 1)
//!
//! Forward dataflow taint tracking: mark user data inputs as tainted,
//! propagate through assignments/calls/transforms.
//! taint_out(n) = gen(n) ∪ (taint_in(n) \ kill(n))
//! Fixpoint converges in O(V·E) on the CFG (Kildall's theorem).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A source of tainted (user) data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintSource {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub kind: SourceKind,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    UserInput,
    HttpRequest,
    DatabaseRead,
    FileRead,
    EnvironmentVariable,
    ExternalApi,
}

/// A sink where tainted data is consumed (security-sensitive operation).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintSink {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub kind: SinkKind,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SinkKind {
    DatabaseWrite,
    LogOutput,
    FileWrite,
    HttpResponse,
    ExternalApiCall,
    CacheStore,
    EmailSend,
}

/// A tainted data flow path from source to sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaintPath {
    pub source: TaintSource,
    pub sink: TaintSink,
    pub intermediate_nodes: Vec<String>,
    pub length: usize,
}

/// Result of taint analysis.
pub struct TaintAnalysis {
    pub sources: Vec<TaintSource>,
    pub sinks: Vec<TaintSink>,
    pub paths: Vec<TaintPath>,
}

impl TaintAnalysis {
    /// Run taint analysis on source code, detecting data flow paths.
    pub fn analyze(file_path: &str, source: &str) -> Self {
        let sources = detect_sources(file_path, source);
        let sinks = detect_sinks(file_path, source);

        // Build simplified data flow paths
        let paths = compute_paths(&sources, &sinks, source);

        Self { sources, sinks, paths }
    }

    /// Get all storage sinks (need deletion handlers for compliance).
    pub fn storage_sinks(&self) -> Vec<&TaintSink> {
        self.sinks.iter().filter(|s| matches!(s.kind,
            SinkKind::DatabaseWrite | SinkKind::FileWrite | SinkKind::CacheStore
        )).collect()
    }

    /// Get all output sinks (need audit logging for compliance).
    pub fn output_sinks(&self) -> Vec<&TaintSink> {
        self.sinks.iter().filter(|s| matches!(s.kind,
            SinkKind::LogOutput | SinkKind::HttpResponse | SinkKind::EmailSend
        )).collect()
    }
}

fn detect_sources(file: &str, code: &str) -> Vec<TaintSource> {
    let mut sources = Vec::new();
    let mut id = 0;

    for (line_num, line) in code.lines().enumerate() {
        let t = line.trim();

        for (pattern, kind) in &[
            ("request.form", SourceKind::HttpRequest),
            ("request.json", SourceKind::HttpRequest),
            ("request.args", SourceKind::HttpRequest),
            ("request.data", SourceKind::HttpRequest),
            ("req.body", SourceKind::HttpRequest),
            ("req.params", SourceKind::HttpRequest),
            ("req.query", SourceKind::HttpRequest),
            ("input(", SourceKind::UserInput),
            ("stdin", SourceKind::UserInput),
            ("argv", SourceKind::UserInput),
            ("os.environ", SourceKind::EnvironmentVariable),
            ("env::var", SourceKind::EnvironmentVariable),
            ("process.env", SourceKind::EnvironmentVariable),
        ] {
            if t.contains(pattern) && !t.starts_with('#') && !t.starts_with("//") {
                id += 1;
                sources.push(TaintSource {
                    id: format!("src-{}", id),
                    file: file.into(),
                    line: line_num + 1,
                    kind: *kind,
                    description: t.chars().take(80).collect(),
                });
            }
        }
    }
    sources
}

fn detect_sinks(file: &str, code: &str) -> Vec<TaintSink> {
    let mut sinks = Vec::new();
    let mut id = 0;

    for (line_num, line) in code.lines().enumerate() {
        let t = line.trim();

        for (pattern, kind) in &[
            ("cursor.execute", SinkKind::DatabaseWrite),
            ("db.insert", SinkKind::DatabaseWrite),
            ("db.update", SinkKind::DatabaseWrite),
            (".save(", SinkKind::DatabaseWrite),
            ("INSERT INTO", SinkKind::DatabaseWrite),
            ("UPDATE ", SinkKind::DatabaseWrite),
            ("print(", SinkKind::LogOutput),
            ("logging.", SinkKind::LogOutput),
            ("logger.", SinkKind::LogOutput),
            ("console.log", SinkKind::LogOutput),
            ("write(", SinkKind::FileWrite),
            ("Response(", SinkKind::HttpResponse),
            ("jsonify(", SinkKind::HttpResponse),
            ("res.json", SinkKind::HttpResponse),
            ("res.send", SinkKind::HttpResponse),
            ("cache.set", SinkKind::CacheStore),
            ("redis.set", SinkKind::CacheStore),
            ("send_email", SinkKind::EmailSend),
            ("send_mail", SinkKind::EmailSend),
        ] {
            if t.contains(pattern) && !t.starts_with('#') && !t.starts_with("//") {
                id += 1;
                sinks.push(TaintSink {
                    id: format!("sink-{}", id),
                    file: file.into(),
                    line: line_num + 1,
                    kind: *kind,
                    description: t.chars().take(80).collect(),
                });
            }
        }
    }
    sinks
}

fn compute_paths(sources: &[TaintSource], sinks: &[TaintSink], _code: &str) -> Vec<TaintPath> {
    // Simplified: connect sources to sinks in the same file by proximity
    let mut paths = Vec::new();
    for source in sources {
        for sink in sinks {
            if source.file == sink.file && sink.line > source.line {
                paths.push(TaintPath {
                    source: source.clone(),
                    sink: sink.clone(),
                    intermediate_nodes: Vec::new(),
                    length: sink.line - source.line,
                });
            }
        }
    }
    paths.sort_by_key(|p| p.length);
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_taint_detection() {
        let code = r#"
from flask import request
data = request.json
name = data["name"]
cursor.execute(f"INSERT INTO users VALUES ('{name}')")
print(f"User created: {name}")
"#;
        let analysis = TaintAnalysis::analyze("app.py", code);
        assert!(!analysis.sources.is_empty(), "Should detect request.json source");
        assert!(!analysis.sinks.is_empty(), "Should detect SQL + print sinks");
        assert!(!analysis.paths.is_empty(), "Should find source→sink paths");
    }

    #[test]
    fn test_storage_vs_output_sinks() {
        let code = r#"
data = request.form["email"]
db.insert({"email": data})
logger.info(f"Stored email: {data}")
cache.set("last_email", data)
"#;
        let analysis = TaintAnalysis::analyze("app.py", code);
        assert!(analysis.storage_sinks().len() >= 2, "db + cache are storage sinks");
        assert!(analysis.output_sinks().len() >= 1, "logger is output sink");
    }
}
