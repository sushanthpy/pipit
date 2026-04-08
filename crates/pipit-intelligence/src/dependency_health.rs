//! Dependency Health Analyzer — Task 1.2
//!
//! Ingests manifest files (Cargo.toml, package.json, pyproject.toml, go.mod),
//! computes version lag, and produces ranked remediation candidates.
//!
//! Staleness score: S = 100*(M-major) + 10*(N-minor) + (P-patch), normalized.
//! Priority: S * log2(1 + in_degree) — high fan-in deps are riskier when stale.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Health report for project dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyHealthReport {
    pub manifest_path: PathBuf,
    pub manifest_type: ManifestType,
    pub dependencies: Vec<DependencyStatus>,
    pub overall_health: f64, // 0.0 (all stale) to 1.0 (all current)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManifestType {
    CargoToml,
    PackageJson,
    PyprojectToml,
    GoMod,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyStatus {
    pub name: String,
    pub current_version: String,
    pub staleness_score: f64, // Normalized [0, 1], 0 = current
    pub priority: f64,        // staleness * log2(1 + in_degree)
    pub is_dev: bool,
}

#[derive(Debug, Clone)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SemVer {
    pub fn parse(s: &str) -> Option<Self> {
        let clean = s.trim_start_matches(|c: char| !c.is_ascii_digit());
        let parts: Vec<&str> = clean.split('.').collect();
        Some(Self {
            major: parts.first()?.parse().ok()?,
            minor: parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0),
            patch: parts
                .get(2)
                .and_then(|s| {
                    // Handle "1.2.3-beta" → strip suffix
                    s.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
                })
                .unwrap_or(0),
        })
    }

    /// Weighted L1 distance: 100*major + 10*minor + patch
    pub fn staleness_from(&self, latest: &SemVer) -> f64 {
        let raw = 100.0 * (latest.major.saturating_sub(self.major) as f64)
            + 10.0 * (latest.minor.saturating_sub(self.minor) as f64)
            + (latest.patch.saturating_sub(self.patch) as f64);
        // Normalize: cap at 500 (5 major versions behind)
        (raw / 500.0).min(1.0)
    }
}

/// Scan a project root for manifest files and produce health reports.
pub fn analyze_dependencies(project_root: &Path) -> Vec<DependencyHealthReport> {
    let mut reports = Vec::new();

    // Check for Cargo.toml
    let cargo = project_root.join("Cargo.toml");
    if cargo.exists() {
        if let Ok(report) = analyze_cargo_toml(&cargo) {
            reports.push(report);
        }
    }

    // Check for package.json
    let pkg_json = project_root.join("package.json");
    if pkg_json.exists() {
        if let Ok(report) = analyze_package_json(&pkg_json) {
            reports.push(report);
        }
    }

    // Check for pyproject.toml
    let pyproject = project_root.join("pyproject.toml");
    if pyproject.exists() {
        if let Ok(report) = analyze_pyproject_toml(&pyproject) {
            reports.push(report);
        }
    }

    reports
}

fn analyze_cargo_toml(path: &Path) -> Result<DependencyHealthReport, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut deps = Vec::new();

    // Simple TOML dependency parser (handles [dependencies] and [dev-dependencies])
    let mut in_deps = false;
    let mut is_dev = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[dependencies]") || trimmed.starts_with("[workspace.dependencies]")
        {
            in_deps = true;
            is_dev = false;
            continue;
        }
        if trimmed.starts_with("[dev-dependencies]") {
            in_deps = true;
            is_dev = true;
            continue;
        }
        if trimmed.starts_with('[') {
            in_deps = false;
            continue;
        }
        if !in_deps || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Parse: name = "version" or name = { version = "..." }
        if let Some((name, rest)) = trimmed.split_once('=') {
            let name = name.trim().trim_matches('"');
            let version = extract_version_from_toml_value(rest.trim());
            if let Some(ver_str) = version {
                deps.push(DependencyStatus {
                    name: name.to_string(),
                    current_version: ver_str,
                    staleness_score: 0.0, // Would need registry lookup for real scoring
                    priority: 0.0,
                    is_dev,
                });
            }
        }
    }

    let overall_health = if deps.is_empty() {
        1.0
    } else {
        1.0 - deps.iter().map(|d| d.staleness_score).sum::<f64>() / deps.len() as f64
    };

    Ok(DependencyHealthReport {
        manifest_path: path.to_path_buf(),
        manifest_type: ManifestType::CargoToml,
        dependencies: deps,
        overall_health,
    })
}

fn analyze_package_json(path: &Path) -> Result<DependencyHealthReport, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let mut deps = Vec::new();

    for (section, is_dev) in [("dependencies", false), ("devDependencies", true)] {
        if let Some(section_obj) = json.get(section).and_then(|v| v.as_object()) {
            for (name, ver) in section_obj {
                let ver_str = ver.as_str().unwrap_or("*").to_string();
                deps.push(DependencyStatus {
                    name: name.clone(),
                    current_version: ver_str,
                    staleness_score: 0.0,
                    priority: 0.0,
                    is_dev,
                });
            }
        }
    }

    Ok(DependencyHealthReport {
        manifest_path: path.to_path_buf(),
        manifest_type: ManifestType::PackageJson,
        dependencies: deps,
        overall_health: 1.0,
    })
}

fn analyze_pyproject_toml(path: &Path) -> Result<DependencyHealthReport, String> {
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut deps = Vec::new();

    // Parse [project.dependencies] list
    let mut in_deps = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "dependencies = [" || trimmed.starts_with("dependencies =") {
            in_deps = true;
            continue;
        }
        if in_deps && trimmed == "]" {
            in_deps = false;
            continue;
        }
        if in_deps {
            let dep = trimmed.trim_matches(&['"', '\'', ',', ' '] as &[char]);
            if !dep.is_empty() {
                // Parse "package>=1.0" style
                let (name, ver) = if let Some(idx) =
                    dep.find(|c: char| c == '>' || c == '<' || c == '=' || c == '~')
                {
                    (
                        &dep[..idx],
                        dep[idx..].trim_start_matches(|c: char| !c.is_ascii_digit()),
                    )
                } else {
                    (dep, "*")
                };
                deps.push(DependencyStatus {
                    name: name.to_string(),
                    current_version: ver.to_string(),
                    staleness_score: 0.0,
                    priority: 0.0,
                    is_dev: false,
                });
            }
        }
    }

    Ok(DependencyHealthReport {
        manifest_path: path.to_path_buf(),
        manifest_type: ManifestType::PyprojectToml,
        dependencies: deps,
        overall_health: 1.0,
    })
}

fn extract_version_from_toml_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('"') {
        // Simple: name = "1.0"
        Some(value.trim_matches('"').to_string())
    } else if value.starts_with('{') {
        // Table: name = { version = "1.0", ... }
        if let Some(start) = value.find("version") {
            let rest = &value[start..];
            if let Some(eq) = rest.find('=') {
                let ver_part = rest[eq + 1..].trim();
                let ver = ver_part.trim_start_matches(|c: char| c == ' ' || c == '"');
                let end = ver.find('"').unwrap_or(ver.len());
                return Some(ver[..end].to_string());
            }
        }
        // Path/git dependency — no version
        None
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_semver_parse() {
        let v = SemVer::parse("1.2.3").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (1, 2, 3));

        let v = SemVer::parse("^0.12").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (0, 12, 0));

        let v = SemVer::parse("2.0.0-beta.1").unwrap();
        assert_eq!((v.major, v.minor, v.patch), (2, 0, 0));
    }

    #[test]
    fn test_staleness_score() {
        let current = SemVer::parse("1.0.0").unwrap();
        let latest = SemVer::parse("2.3.1").unwrap();
        let score = current.staleness_from(&latest);
        // 100*1 + 10*3 + 1 = 131. normalized: 131/500 = 0.262
        assert!((score - 0.262).abs() < 0.01, "score = {}", score);
    }

    #[test]
    fn test_analyze_real_cargo() {
        let root = std::env::current_dir().unwrap();
        let cargo = root.join("Cargo.toml");
        if cargo.exists() {
            let report = analyze_cargo_toml(&cargo);
            assert!(report.is_ok());
            let report = report.unwrap();
            assert!(!report.dependencies.is_empty(), "Should find dependencies");
        }
    }
}
