//! Adversarial Threat Model Generator — Task 4.1
//!
//! Decomposes a codebase into attack surfaces using STRIDE-mapped patterns.
//! Threat score: threat(v) = Σ_{paths} (1/length(p)) · criticality(v)
//! — variant of betweenness centrality weighted by path length.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// STRIDE threat categories mapped to code patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StrideCategory {
    Spoofing,
    Tampering,
    Repudiation,
    InformationDisclosure,
    DenialOfService,
    ElevationOfPrivilege,
}

/// An attack surface node in the threat graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttackSurface {
    pub id: String,
    pub file: String,
    pub line: usize,
    pub kind: SurfaceKind,
    pub criticality: f64,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceKind {
    HttpEndpoint,
    CliArgument,
    FileParser,
    DatabaseQuery,
    ExternalApiCall,
    AuthCheck,
    CryptoOperation,
    FileSystemAccess,
    ProcessExecution,
    Deserialization,
}

impl SurfaceKind {
    pub fn criticality(&self) -> f64 {
        match self {
            Self::ProcessExecution => 1.0,
            Self::DatabaseQuery => 0.9,
            Self::AuthCheck => 0.9,
            Self::CryptoOperation => 0.85,
            Self::Deserialization => 0.8,
            Self::HttpEndpoint => 0.7,
            Self::FileSystemAccess => 0.7,
            Self::ExternalApiCall => 0.6,
            Self::FileParser => 0.5,
            Self::CliArgument => 0.4,
        }
    }
}

/// A potential threat with exploit chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Threat {
    pub surface: AttackSurface,
    pub stride_categories: Vec<StrideCategory>,
    pub threat_score: f64,
    pub exploit_chain: Vec<String>,
    pub mitigation: String,
    pub severity: Severity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl From<f64> for Severity {
    fn from(score: f64) -> Self {
        if score >= 0.8 {
            Self::Critical
        } else if score >= 0.6 {
            Self::High
        } else if score >= 0.3 {
            Self::Medium
        } else {
            Self::Low
        }
    }
}

/// Full threat report for a codebase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatReport {
    pub surfaces: Vec<AttackSurface>,
    pub threats: Vec<Threat>,
    pub summary: ThreatSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatSummary {
    pub total_surfaces: usize,
    pub total_threats: usize,
    pub critical_count: usize,
    pub high_count: usize,
    pub medium_count: usize,
    pub low_count: usize,
}

/// Detect attack surfaces from source code patterns.
/// Uses comment/string awareness to reduce false positives.
pub fn detect_attack_surfaces(file_path: &str, source: &str) -> Vec<AttackSurface> {
    let mut surfaces = Vec::new();
    let mut id_counter = 0;
    let mut in_block_comment = false;

    for (line_num, line) in source.lines().enumerate() {
        let trimmed = line.trim();

        // Track block comments (/* ... */ and """ ... """)
        if trimmed.contains("/*") && !trimmed.contains("*/") {
            in_block_comment = true;
            continue;
        }
        if trimmed.contains("*/") {
            in_block_comment = false;
            continue;
        }
        if trimmed.starts_with("\"\"\"") || trimmed.starts_with("'''") {
            in_block_comment = !in_block_comment;
            continue;
        }
        if in_block_comment {
            continue;
        }

        // Skip line comments and pure string literals
        if is_comment_or_string(trimmed) {
            continue;
        }

        // HTTP endpoints
        for pattern in &[
            "#[get(",
            "#[post(",
            "#[put(",
            "#[delete(",
            "@app.route",
            "@router.",
            "app.get(",
            "app.post(",
            "router.get(",
            "router.post(",
        ] {
            if trimmed.contains(pattern) {
                id_counter += 1;
                surfaces.push(AttackSurface {
                    id: format!("{}:{}", file_path, id_counter),
                    file: file_path.to_string(),
                    line: line_num + 1,
                    kind: SurfaceKind::HttpEndpoint,
                    criticality: SurfaceKind::HttpEndpoint.criticality(),
                    description: format!("HTTP endpoint: {}", trimmed),
                });
            }
        }

        // SQL / database queries
        for pattern in &[
            "execute(",
            "query(",
            "raw_sql",
            "cursor.execute",
            "SELECT ",
            "INSERT ",
            "UPDATE ",
            "DELETE ",
            "db.query",
            "pool.query",
        ] {
            if trimmed.contains(pattern) && !trimmed.starts_with("//") && !trimmed.starts_with('#')
            {
                id_counter += 1;
                surfaces.push(AttackSurface {
                    id: format!("{}:{}", file_path, id_counter),
                    file: file_path.to_string(),
                    line: line_num + 1,
                    kind: SurfaceKind::DatabaseQuery,
                    criticality: SurfaceKind::DatabaseQuery.criticality(),
                    description: format!(
                        "Database operation: {}",
                        trimmed.chars().take(80).collect::<String>()
                    ),
                });
            }
        }

        // Process execution
        for pattern in &[
            "Command::new",
            "subprocess.run",
            "subprocess.Popen",
            "exec(",
            "eval(",
            "os.system(",
            "shell=True",
            "Process.Start",
        ] {
            if trimmed.contains(pattern) && !trimmed.starts_with("//") && !trimmed.starts_with('#')
            {
                id_counter += 1;
                surfaces.push(AttackSurface {
                    id: format!("{}:{}", file_path, id_counter),
                    file: file_path.to_string(),
                    line: line_num + 1,
                    kind: SurfaceKind::ProcessExecution,
                    criticality: SurfaceKind::ProcessExecution.criticality(),
                    description: format!(
                        "Process execution: {}",
                        trimmed.chars().take(80).collect::<String>()
                    ),
                });
            }
        }

        // Deserialization
        for pattern in &[
            "from_str(",
            "from_slice(",
            "deserialize(",
            "json.loads(",
            "pickle.load",
            "yaml.load(",
            "JSON.parse(",
            "eval(",
        ] {
            if trimmed.contains(pattern) && !trimmed.starts_with("//") && !trimmed.starts_with('#')
            {
                id_counter += 1;
                surfaces.push(AttackSurface {
                    id: format!("{}:{}", file_path, id_counter),
                    file: file_path.to_string(),
                    line: line_num + 1,
                    kind: SurfaceKind::Deserialization,
                    criticality: SurfaceKind::Deserialization.criticality(),
                    description: format!(
                        "Deserialization: {}",
                        trimmed.chars().take(80).collect::<String>()
                    ),
                });
            }
        }

        // File system access
        for pattern in &[
            "open(",
            "read_to_string",
            "write(",
            "fs::",
            "std::fs",
            "Path::new",
            "pathlib.Path",
        ] {
            if trimmed.contains(pattern)
                && !trimmed.starts_with("//")
                && !trimmed.starts_with('#')
                && !trimmed.contains("test")
            {
                id_counter += 1;
                surfaces.push(AttackSurface {
                    id: format!("{}:{}", file_path, id_counter),
                    file: file_path.to_string(),
                    line: line_num + 1,
                    kind: SurfaceKind::FileSystemAccess,
                    criticality: SurfaceKind::FileSystemAccess.criticality(),
                    description: format!(
                        "File access: {}",
                        trimmed.chars().take(80).collect::<String>()
                    ),
                });
            }
        }
    }

    surfaces
}

/// Compute threat scores using BFS from untrusted input nodes.
/// threat(v) = Σ_{paths p from input to v} (1/length(p)) · criticality(v)
pub fn compute_threat_scores(
    surfaces: &[AttackSurface],
    data_flow_edges: &[(usize, usize)],
    input_nodes: &[usize],
) -> Vec<f64> {
    let n = surfaces.len();
    let mut scores = vec![0.0_f64; n];

    // Build adjacency list
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for &(from, to) in data_flow_edges {
        if from < n && to < n {
            adj[from].push(to);
        }
    }

    // BFS from each input node
    for &start in input_nodes {
        if start >= n {
            continue;
        }
        let mut visited = vec![false; n];
        let mut queue = VecDeque::new();
        queue.push_back((start, 0usize));
        visited[start] = true;

        while let Some((node, depth)) = queue.pop_front() {
            if depth > 0 {
                let path_weight = 1.0 / (depth as f64);
                scores[node] += path_weight * surfaces[node].criticality;
            }

            for &next in &adj[node] {
                if !visited[next] {
                    visited[next] = true;
                    queue.push_back((next, depth + 1));
                }
            }
        }
    }

    scores
}

/// Map attack surfaces to STRIDE categories.
pub fn classify_stride(surface: &AttackSurface) -> Vec<StrideCategory> {
    match surface.kind {
        SurfaceKind::HttpEndpoint => vec![
            StrideCategory::Spoofing,
            StrideCategory::Tampering,
            StrideCategory::DenialOfService,
        ],
        SurfaceKind::DatabaseQuery => vec![
            StrideCategory::Tampering,
            StrideCategory::InformationDisclosure,
        ],
        SurfaceKind::ProcessExecution => vec![
            StrideCategory::ElevationOfPrivilege,
            StrideCategory::Tampering,
        ],
        SurfaceKind::Deserialization => vec![
            StrideCategory::Tampering,
            StrideCategory::ElevationOfPrivilege,
        ],
        SurfaceKind::AuthCheck => vec![
            StrideCategory::Spoofing,
            StrideCategory::ElevationOfPrivilege,
        ],
        SurfaceKind::CryptoOperation => vec![
            StrideCategory::InformationDisclosure,
            StrideCategory::Repudiation,
        ],
        SurfaceKind::FileSystemAccess => vec![
            StrideCategory::Tampering,
            StrideCategory::InformationDisclosure,
        ],
        SurfaceKind::ExternalApiCall => {
            vec![StrideCategory::Spoofing, StrideCategory::DenialOfService]
        }
        SurfaceKind::FileParser => vec![StrideCategory::Tampering, StrideCategory::DenialOfService],
        SurfaceKind::CliArgument => vec![StrideCategory::Tampering],
    }
}

/// Build a complete threat report.
pub fn generate_threat_report(surfaces: Vec<AttackSurface>) -> ThreatReport {
    let threats: Vec<Threat> = surfaces
        .iter()
        .map(|s| {
            let categories = classify_stride(s);
            let severity = Severity::from(s.criticality);
            Threat {
                surface: s.clone(),
                stride_categories: categories,
                threat_score: s.criticality,
                exploit_chain: vec![format!(
                    "Input reaches {} at {}:{}",
                    s.kind_str(),
                    s.file,
                    s.line
                )],
                mitigation: suggest_mitigation(s),
                severity,
            }
        })
        .collect();

    let summary = ThreatSummary {
        total_surfaces: surfaces.len(),
        total_threats: threats.len(),
        critical_count: threats
            .iter()
            .filter(|t| t.severity == Severity::Critical)
            .count(),
        high_count: threats
            .iter()
            .filter(|t| t.severity == Severity::High)
            .count(),
        medium_count: threats
            .iter()
            .filter(|t| t.severity == Severity::Medium)
            .count(),
        low_count: threats
            .iter()
            .filter(|t| t.severity == Severity::Low)
            .count(),
    };

    ThreatReport {
        surfaces,
        threats,
        summary,
    }
}

impl AttackSurface {
    fn kind_str(&self) -> &str {
        match self.kind {
            SurfaceKind::HttpEndpoint => "HTTP endpoint",
            SurfaceKind::DatabaseQuery => "database query",
            SurfaceKind::ProcessExecution => "process execution",
            SurfaceKind::Deserialization => "deserialization",
            SurfaceKind::AuthCheck => "auth check",
            SurfaceKind::CryptoOperation => "crypto operation",
            SurfaceKind::FileSystemAccess => "file access",
            SurfaceKind::ExternalApiCall => "external API",
            SurfaceKind::FileParser => "file parser",
            SurfaceKind::CliArgument => "CLI argument",
        }
    }
}

fn suggest_mitigation(surface: &AttackSurface) -> String {
    match surface.kind {
        SurfaceKind::ProcessExecution => "Validate and sanitize all inputs before command execution. Use allowlists. Avoid shell=True.".into(),
        SurfaceKind::DatabaseQuery => "Use parameterized queries. Never interpolate user input into SQL strings.".into(),
        SurfaceKind::Deserialization => "Validate input before deserialization. Use safe deserializers. Avoid pickle/eval.".into(),
        SurfaceKind::HttpEndpoint => "Validate all request parameters. Implement rate limiting. Use CSRF tokens.".into(),
        SurfaceKind::FileSystemAccess => "Validate file paths. Prevent path traversal. Use chroot/sandbox.".into(),
        SurfaceKind::AuthCheck => "Use constant-time comparison. Implement proper session management.".into(),
        SurfaceKind::CryptoOperation => "Use well-tested crypto libraries. Never roll your own crypto.".into(),
        _ => "Review for input validation and access control.".into(),
    }
}

/// Check if a line is a comment or inside a string literal.
fn is_comment_or_string(trimmed: &str) -> bool {
    // Line comments
    if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("--") {
        return true;
    }
    // Pure string assignment (the pattern is INSIDE a string, not actual code)
    if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() > 2 {
        return true;
    }
    if trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() > 2 {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_surfaces() {
        let code = r#"
from flask import Flask
app = Flask(__name__)

@app.route('/users', methods=['POST'])
def create_user():
    data = json.loads(request.data)
    cursor.execute(f"INSERT INTO users VALUES ({data['name']})")
    subprocess.run(data['cmd'], shell=True)
"#;
        let surfaces = detect_attack_surfaces("app.py", code);
        assert!(
            surfaces.len() >= 3,
            "Should find HTTP, SQL, subprocess: found {}",
            surfaces.len()
        );

        let kinds: Vec<_> = surfaces.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&SurfaceKind::HttpEndpoint));
        assert!(kinds.contains(&SurfaceKind::DatabaseQuery));
        assert!(kinds.contains(&SurfaceKind::ProcessExecution));
    }

    #[test]
    fn test_threat_scoring() {
        let surfaces = vec![
            AttackSurface {
                id: "1".into(),
                file: "a.py".into(),
                line: 1,
                kind: SurfaceKind::HttpEndpoint,
                criticality: 0.7,
                description: "endpoint".into(),
            },
            AttackSurface {
                id: "2".into(),
                file: "a.py".into(),
                line: 5,
                kind: SurfaceKind::DatabaseQuery,
                criticality: 0.9,
                description: "query".into(),
            },
        ];
        let edges = vec![(0, 1)]; // HTTP → DB
        let inputs = vec![0]; // HTTP is input

        let scores = compute_threat_scores(&surfaces, &edges, &inputs);
        assert!(
            scores[1] > 0.0,
            "DB should get threat score from HTTP input"
        );
        assert_eq!(scores[0], 0.0, "Input node itself gets 0 (depth=0)");
    }

    #[test]
    fn test_stride_classification() {
        let surface = AttackSurface {
            id: "1".into(),
            file: "a.py".into(),
            line: 1,
            kind: SurfaceKind::ProcessExecution,
            criticality: 1.0,
            description: "exec".into(),
        };
        let cats = classify_stride(&surface);
        assert!(cats.contains(&StrideCategory::ElevationOfPrivilege));
    }
}
