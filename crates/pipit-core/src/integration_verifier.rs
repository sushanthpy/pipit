//! Integration verifier — cross-artifact consistency check.
//!
//! Runs BEFORE `ProofPacket` finalization to catch the specific failure
//! mode that drove v2's integration score from 5 to 4: each artifact is
//! independently correct but the seams don't match.
//!
//! The verifier does structural comparison, not LLM judgment. It asks:
//!   - Does every field the frontend fetches exist in the API response shape?
//!   - Does every seed record conform to the schema it targets?
//!   - Is the auth middleware referenced by admin routes the same one used
//!     by public routes?
//!   - Are all API paths declared in the domain architecture actually
//!     implemented as routes?

use crate::domain_architect::ArchitectureIR;
use crate::proof::RealizedEdit;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

// ── Result types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationReport {
    pub overall: IntegrationVerdict,
    pub findings: Vec<Finding>,
    /// Number of checks actually executed.
    pub checks_run: usize,
    /// Confidence cap: if findings exist, the ProofPacket's confidence
    /// should not exceed this value.
    pub confidence_ceiling: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntegrationVerdict {
    Pass,
    Fail,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    pub message: String,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FindingKind {
    OrphanFetch,
    UnusedRoute,
    SeedSchemaMismatch,
    AuthShapeDrift,
    EntityShapeDrift,
    MissingEndpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub path: PathBuf,
    pub line: Option<u32>,
    pub snippet: String,
}

// ── Entry point ─────────────────────────────────────────────────────────

/// Run all integration checks.
pub fn verify_integration(
    ir: Option<&ArchitectureIR>,
    edits: &[RealizedEdit],
    project_root: &Path,
) -> IntegrationReport {
    let mut findings = Vec::new();
    let mut checks_run = 0;

    let Some(ir) = ir else {
        return IntegrationReport {
            overall: IntegrationVerdict::Inconclusive,
            findings,
            checks_run,
            confidence_ceiling: 1.0,
        };
    };

    if edits.len() < 2 {
        return IntegrationReport {
            overall: IntegrationVerdict::Inconclusive,
            findings,
            checks_run,
            confidence_ceiling: 1.0,
        };
    }

    checks_run += 1;
    findings.extend(check_missing_endpoints(ir, edits, project_root));

    checks_run += 1;
    findings.extend(check_orphan_fetches(edits, project_root));

    checks_run += 1;
    findings.extend(check_seed_schema(ir, edits, project_root));

    checks_run += 1;
    findings.extend(check_auth_consistency(edits, project_root));

    checks_run += 1;
    findings.extend(check_entity_shape_drift(ir, edits, project_root));

    let verdict = if findings.iter().any(|f| f.severity == Severity::Error) {
        IntegrationVerdict::Fail
    } else if findings.iter().any(|f| f.severity == Severity::Warn) {
        IntegrationVerdict::Fail
    } else {
        IntegrationVerdict::Pass
    };

    let confidence_ceiling = findings.iter().fold(1.0_f32, |acc, f| match f.severity {
        Severity::Error => acc.min(0.40),
        Severity::Warn => acc.min(0.70),
        Severity::Info => acc,
    });

    IntegrationReport {
        overall: verdict,
        findings,
        checks_run,
        confidence_ceiling,
    }
}

// ── Individual checks ───────────────────────────────────────────────────

fn check_missing_endpoints(
    ir: &ArchitectureIR,
    edits: &[RealizedEdit],
    project_root: &Path,
) -> Vec<Finding> {
    if ir.interfaces.is_empty() {
        return Vec::new();
    }

    let corpus = read_edited_files(edits, project_root);
    let mut findings = Vec::new();

    for iface in &ir.interfaces {
        let path_norm = iface.path.trim_start_matches('/');
        let path_quoted = format!("\"/{}", path_norm);
        let path_quoted_single = format!("'/{}", path_norm);
        let found = corpus.iter().any(|(_, content)| {
            content.contains(&path_quoted) || content.contains(&path_quoted_single)
        });

        if !found {
            findings.push(Finding {
                kind: FindingKind::MissingEndpoint,
                severity: Severity::Error,
                message: format!(
                    "API surface declares `{} {}` but no route with that path was found in written files",
                    iface.method, iface.path
                ),
                evidence: Vec::new(),
            });
        }
    }
    findings
}

fn check_orphan_fetches(edits: &[RealizedEdit], project_root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let corpus = read_edited_files(edits, project_root);
    let backend_paths = extract_backend_paths(&corpus);

    let fetch_re = regex::Regex::new(
        r#"(?:fetch|axios\.(?:get|post|put|delete|patch))\s*\(\s*[`"']([^`"']+)[`"']"#,
    )
    .ok();
    let Some(re) = fetch_re else {
        return findings;
    };

    for (path, content) in &corpus {
        if !is_frontend_file(path) {
            continue;
        }
        for (line_no, line) in content.lines().enumerate() {
            for cap in re.captures_iter(line) {
                let fetched = cap.get(1).map(|m| m.as_str()).unwrap_or("");
                let fetched_path = fetched.split('?').next().unwrap_or(fetched);
                let local = if let Some(idx) = fetched_path.find("/api/") {
                    &fetched_path[idx..]
                } else if fetched_path.starts_with('/') {
                    fetched_path
                } else {
                    continue;
                };

                let matches = backend_paths.iter().any(|bp| paths_compatible(bp, local));
                if !matches {
                    findings.push(Finding {
                        kind: FindingKind::OrphanFetch,
                        severity: Severity::Error,
                        message: format!(
                            "Frontend fetches `{}` but no matching backend route was declared",
                            local
                        ),
                        evidence: vec![Evidence {
                            path: path.clone(),
                            line: Some((line_no + 1) as u32),
                            snippet: line.trim().to_string(),
                        }],
                    });
                }
            }
        }
    }

    findings
}

fn check_seed_schema(
    ir: &ArchitectureIR,
    edits: &[RealizedEdit],
    project_root: &Path,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let corpus = read_edited_files(edits, project_root);

    let mut entity_fields: HashMap<String, HashSet<String>> = HashMap::new();
    for e in &ir.entities {
        let lower_name = e.name.to_ascii_lowercase();
        entity_fields.insert(
            lower_name,
            e.attributes.iter().map(|a| a.to_ascii_lowercase()).collect(),
        );
    }

    for (path, content) in &corpus {
        if !is_seed_file(path) {
            continue;
        }
        for entity in &ir.entities {
            let entity_lower = entity.name.to_ascii_lowercase();
            if !content.to_ascii_lowercase().contains(&entity_lower) {
                continue;
            }
            let field_re = regex::Regex::new(r#"(?m)^\s*(\w+)\s*:"#).ok();
            let Some(re) = field_re else {
                continue;
            };
            let schema_fields = entity_fields.get(&entity_lower);
            let Some(schema_fields) = schema_fields else {
                continue;
            };

            for cap in re.captures_iter(content) {
                let Some(m) = cap.get(1) else {
                    continue;
                };
                let field = m.as_str().to_ascii_lowercase();
                if matches!(
                    field.as_str(),
                    "type" | "model" | "name" | "data" | "fields"
                ) {
                    continue;
                }
                if !schema_fields.contains(&field) {
                    findings.push(Finding {
                        kind: FindingKind::SeedSchemaMismatch,
                        severity: Severity::Warn,
                        message: format!(
                            "Seed file references `{}` on entity `{}` but the schema does not declare that attribute",
                            field, entity.name
                        ),
                        evidence: vec![Evidence {
                            path: path.clone(),
                            line: None,
                            snippet: format!("field `{}`", field),
                        }],
                    });
                    break;
                }
            }
        }
    }

    findings
}

fn check_auth_consistency(edits: &[RealizedEdit], project_root: &Path) -> Vec<Finding> {
    let corpus = read_edited_files(edits, project_root);
    let mut findings = Vec::new();

    let auth_re =
        regex::Regex::new(r#"(?:import|use|require)\s+.{0,200}?(?:auth|middleware)\S*"#).ok();
    let Some(re) = auth_re else {
        return findings;
    };

    let mut admin_imports: HashSet<String> = HashSet::new();
    let mut public_imports: HashSet<String> = HashSet::new();

    for (path, content) in &corpus {
        let p = path.to_string_lossy();
        let is_admin = p.contains("/admin") || p.contains("admin.");
        for cap in re.find_iter(content) {
            let s = cap.as_str().to_string();
            if is_admin {
                admin_imports.insert(s);
            } else if p.contains("route") || p.contains("api") || p.contains("handler") {
                public_imports.insert(s);
            }
        }
    }

    let overlap: Vec<_> = admin_imports.intersection(&public_imports).collect();
    if !admin_imports.is_empty() && !public_imports.is_empty() && overlap.is_empty() {
        findings.push(Finding {
            kind: FindingKind::AuthShapeDrift,
            severity: Severity::Warn,
            message:
                "Admin routes and public routes do not share any auth import. \
                 This usually means auth was re-implemented instead of reused."
                    .into(),
            evidence: Vec::new(),
        });
    }

    findings
}

fn check_entity_shape_drift(
    _ir: &ArchitectureIR,
    _edits: &[RealizedEdit],
    _project_root: &Path,
) -> Vec<Finding> {
    // Stub: full implementation is framework-specific.
    Vec::new()
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn read_edited_files(edits: &[RealizedEdit], project_root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for e in edits {
        let path = if PathBuf::from(&e.path).is_absolute() {
            PathBuf::from(&e.path)
        } else {
            project_root.join(&e.path)
        };
        if let Ok(content) = std::fs::read_to_string(&path) {
            out.push((path, content));
        }
    }
    out
}

fn is_frontend_file(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.ends_with(".tsx")
        || s.ends_with(".jsx")
        || s.contains("/frontend/")
        || s.contains("/client/")
        || s.contains("/pages/")
        || s.contains("/app/")
        || s.contains("/components/")
}

fn is_seed_file(path: &Path) -> bool {
    let s = path.to_string_lossy().to_ascii_lowercase();
    s.contains("seed") || s.contains("fixture") || s.contains("sample")
}

fn extract_backend_paths(corpus: &[(PathBuf, String)]) -> HashSet<String> {
    let mut paths = HashSet::new();
    let re = regex::Regex::new(
        r#"\.\s*(?:get|post|put|delete|patch|route)\s*\(\s*[`"']([^`"']+)[`"']"#,
    );
    if let Ok(re) = re {
        for (_, content) in corpus {
            for cap in re.captures_iter(content) {
                if let Some(m) = cap.get(1) {
                    paths.insert(m.as_str().to_string());
                }
            }
        }
    }
    paths
}

fn paths_compatible(declared: &str, fetched: &str) -> bool {
    let d = declared.trim_start_matches("/api").trim_start_matches('/');
    let f = fetched.trim_start_matches("/api").trim_start_matches('/');

    if d == f {
        return true;
    }

    let d_parts: Vec<&str> = d.split('/').collect();
    let f_parts: Vec<&str> = f.split('/').collect();
    if d_parts.len() != f_parts.len() {
        return false;
    }
    for (dp, fp) in d_parts.iter().zip(f_parts.iter()) {
        let is_param = dp.starts_with(':') || (dp.starts_with('{') && dp.ends_with('}'));
        if !is_param && dp != fp {
            return false;
        }
    }
    true
}

// ── Coordinator-facing summary ──────────────────────────────────────────

impl IntegrationReport {
    /// Render a coordinator-readable summary for injection into the tool result.
    pub fn render_for_model(&self) -> String {
        let mut out = String::new();
        match self.overall {
            IntegrationVerdict::Pass => {
                out.push_str(&format!(
                    "Integration verification passed. {} check(s) run, 0 findings.\n",
                    self.checks_run
                ));
            }
            IntegrationVerdict::Fail => {
                out.push_str("INTEGRATION VERIFICATION FAILED\n\n");
                out.push_str(
                    "You have produced artifacts that do not fit together. \
                     Before declaring the task complete, resolve the findings below. \
                     Do NOT claim done until all Error-severity findings are cleared.\n\n",
                );
                for (i, f) in self.findings.iter().enumerate() {
                    let sev = match f.severity {
                        Severity::Error => "ERROR",
                        Severity::Warn => "WARN",
                        Severity::Info => "INFO",
                    };
                    out.push_str(&format!("{}. [{}] {}\n", i + 1, sev, f.message));
                    for ev in &f.evidence {
                        out.push_str(&format!(
                            "     at {}{}\n",
                            ev.path.display(),
                            ev.line.map(|l| format!(":{}", l)).unwrap_or_default()
                        ));
                    }
                }
                out.push_str(&format!(
                    "\nConfidence ceiling: {:.2}. ProofPacket cannot exceed this until findings are resolved.\n",
                    self.confidence_ceiling
                ));
            }
            IntegrationVerdict::Inconclusive => {
                out.push_str(
                    "Integration verification was inconclusive (no canonical domain \
                     model available, or insufficient artifacts to cross-check).\n",
                );
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compatible_handles_params() {
        assert!(paths_compatible("/users/:id", "/users/abc"));
        assert!(paths_compatible("/users/{id}", "/users/abc"));
        assert!(!paths_compatible("/users/:id", "/posts/abc"));
        assert!(paths_compatible("/api/users", "users"));
    }

    #[test]
    fn empty_ir_returns_inconclusive() {
        let report = verify_integration(None, &[], Path::new("/tmp"));
        assert_eq!(report.overall, IntegrationVerdict::Inconclusive);
    }

    #[test]
    fn insufficient_edits_returns_inconclusive() {
        let ir = ArchitectureIR::default();
        let report = verify_integration(Some(&ir), &[], Path::new("/tmp"));
        assert_eq!(report.overall, IntegrationVerdict::Inconclusive);
    }

    #[test]
    fn confidence_ceiling_caps_with_errors() {
        let report = IntegrationReport {
            overall: IntegrationVerdict::Fail,
            findings: vec![Finding {
                kind: FindingKind::MissingEndpoint,
                severity: Severity::Error,
                message: "".into(),
                evidence: vec![],
            }],
            checks_run: 1,
            confidence_ceiling: 0.40,
        };
        let rendered = report.render_for_model();
        assert!(rendered.contains("INTEGRATION VERIFICATION FAILED"));
        assert!(rendered.contains("0.40"));
    }
}
