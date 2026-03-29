//! pipit-deps: Package Oracle — real-time dependency health monitoring.
//!
//! Scans Cargo.toml, package.json, pyproject.toml, go.mod for:
//! - Version freshness (newer semver-compatible versions available)
//! - Known vulnerabilities (via OSV API)
//! - License compatibility
//! - Deprecation notices
//!
//! Results cache to `.pipit/deps-cache.json` with 1-hour TTL.

pub mod osv;
pub mod scanner;

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Severity level for dependency findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

/// A finding about a dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepFinding {
    pub package: String,
    pub current_version: String,
    pub severity: Severity,
    pub kind: FindingKind,
    pub description: String,
}

/// Kind of dependency finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FindingKind {
    Vulnerability { cve: Option<String>, latest_safe: Option<String> },
    Outdated { latest: String },
    Deprecated { replacement: Option<String> },
    LicenseConflict { dep_license: String, project_license: String },
}

/// Result of a dependency health scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepHealthReport {
    pub total_deps: usize,
    pub findings: Vec<DepFinding>,
    pub scan_duration_ms: u64,
}

impl DepHealthReport {
    /// Format a summary for display.
    pub fn summary(&self) -> String {
        let vulns = self.findings.iter().filter(|f| matches!(f.kind, FindingKind::Vulnerability { .. })).count();
        let outdated = self.findings.iter().filter(|f| matches!(f.kind, FindingKind::Outdated { .. })).count();
        let deprecated = self.findings.iter().filter(|f| matches!(f.kind, FindingKind::Deprecated { .. })).count();

        format!(
            "{} deps scanned in {:.1}s, {} vulnerable, {} deprecated, {} outdated",
            self.total_deps,
            self.scan_duration_ms as f64 / 1000.0,
            vulns,
            deprecated,
            outdated,
        )
    }

    /// Format findings for TUI display.
    pub fn display_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for f in &self.findings {
            let icon = match f.severity {
                Severity::Critical | Severity::High => "🔴",
                Severity::Medium => "🟡",
                Severity::Low | Severity::Info => "🔵",
            };
            lines.push(format!("{} {}@{} — {}", icon, f.package, f.current_version, f.description));
        }
        lines
    }
}

/// Run a full dependency health scan on the project.
pub async fn scan_project(project_root: &Path) -> DepHealthReport {
    let start = std::time::Instant::now();
    let mut findings = Vec::new();
    let mut total_deps = 0;

    // Scan Cargo.toml
    let cargo_path = project_root.join("Cargo.toml");
    if cargo_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&cargo_path) {
            if let Ok(doc) = content.parse::<toml::Value>() {
                if let Some(deps) = doc.get("dependencies").and_then(|d| d.as_table()) {
                    total_deps += deps.len();
                    for (name, spec) in deps {
                        let version = match spec {
                            toml::Value::String(v) => v.clone(),
                            toml::Value::Table(t) => t.get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("*")
                                .to_string(),
                            _ => continue,
                        };
                        // Query OSV for vulnerabilities
                        if let Some(vuln) = osv::check_crate(name, &version).await {
                            findings.push(vuln);
                        }
                    }
                }
            }
        }
    }

    // Scan package.json
    let pkg_path = project_root.join("package.json");
    if pkg_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&pkg_path) {
            if let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&content) {
                for section in ["dependencies", "devDependencies"] {
                    if let Some(deps) = pkg.get(section).and_then(|d| d.as_object()) {
                        total_deps += deps.len();
                        for (name, version) in deps {
                            let ver = version.as_str().unwrap_or("*");
                            if let Some(vuln) = osv::check_npm(name, ver).await {
                                findings.push(vuln);
                            }
                        }
                    }
                }
            }
        }
    }

    let elapsed = start.elapsed().as_millis() as u64;
    DepHealthReport {
        total_deps,
        findings,
        scan_duration_ms: elapsed,
    }
}
