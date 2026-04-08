//! OSV (Open Source Vulnerabilities) API client.
//! https://api.osv.dev/v1/query

use crate::{DepFinding, FindingKind, Severity};

const OSV_API: &str = "https://api.osv.dev/v1/query";

/// Query OSV for vulnerabilities in a crate.
pub async fn check_crate(name: &str, version: &str) -> Option<DepFinding> {
    check_package("crates.io", name, version).await
}

/// Query OSV for vulnerabilities in an npm package.
pub async fn check_npm(name: &str, version: &str) -> Option<DepFinding> {
    let clean_version = version.trim_start_matches('^').trim_start_matches('~');
    check_package("npm", name, clean_version).await
}

async fn check_package(ecosystem: &str, name: &str, version: &str) -> Option<DepFinding> {
    let body = serde_json::json!({
        "version": version,
        "package": {
            "name": name,
            "ecosystem": ecosystem
        }
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let resp = client.post(OSV_API).json(&body).send().await.ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let data: serde_json::Value = resp.json().await.ok()?;
    let vulns = data.get("vulns")?.as_array()?;

    if vulns.is_empty() {
        return None;
    }

    let first = &vulns[0];
    let id = first
        .get("id")
        .and_then(|i| i.as_str())
        .unwrap_or("unknown");
    let summary = first
        .get("summary")
        .and_then(|s| s.as_str())
        .unwrap_or("Vulnerability found");
    let severity_str = first
        .get("database_specific")
        .and_then(|d| d.get("severity"))
        .and_then(|s| s.as_str())
        .unwrap_or("MODERATE");

    let severity = match severity_str.to_uppercase().as_str() {
        "CRITICAL" => Severity::Critical,
        "HIGH" => Severity::High,
        "MODERATE" | "MEDIUM" => Severity::Medium,
        _ => Severity::Low,
    };

    Some(DepFinding {
        package: name.to_string(),
        current_version: version.to_string(),
        severity,
        kind: FindingKind::Vulnerability {
            cve: Some(id.to_string()),
            latest_safe: None,
        },
        description: summary.to_string(),
    })
}
