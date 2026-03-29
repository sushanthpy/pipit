//! Compliance Code Generator — Task 9.2 (part 2)
//!
//! Generates compliance enforcement code from ComplianceRequirement objects:
//! - Deletion handlers per storage sink
//! - Audit log middleware per data access
//! - Retention policy per data persistence

use crate::regulation::{ActionKind, ComplianceRequirement, ComplianceSeverity};
use crate::taint::{TaintAnalysis, SinkKind};
use serde::{Deserialize, Serialize};

/// A planned compliance code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceCodePlan {
    pub requirement_id: String,
    pub changes: Vec<CodeChange>,
    pub priority: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeChange {
    pub file: String,
    pub change_type: ChangeType,
    pub description: String,
    pub code_template: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ChangeType {
    AddDeletionHandler,
    AddAuditLog,
    AddRetentionPolicy,
    AddEncryption,
    AddAccessControl,
    AddConsentCheck,
    AddAnonymization,
}

/// Generate compliance code changes from requirements + taint analysis.
pub fn generate_compliance_code(
    requirements: &[ComplianceRequirement],
    taint: &TaintAnalysis,
) -> Vec<ComplianceCodePlan> {
    let mut plans = Vec::new();

    for req in requirements {
        let mut changes = Vec::new();

        match req.action_required {
            ActionKind::DataDeletion => {
                for sink in taint.storage_sinks() {
                    changes.push(CodeChange {
                        file: sink.file.clone(),
                        change_type: ChangeType::AddDeletionHandler,
                        description: format!(
                            "Add deletion handler for {} at line {} (required by {})",
                            sink.description, sink.line, req.article
                        ),
                        code_template: generate_deletion_template(&sink.kind, &sink.description),
                    });
                }
            }
            ActionKind::AuditLogging => {
                for sink in taint.output_sinks() {
                    changes.push(CodeChange {
                        file: sink.file.clone(),
                        change_type: ChangeType::AddAuditLog,
                        description: format!(
                            "Add audit logging at {} line {} (required by {})",
                            sink.file, sink.line, req.article
                        ),
                        code_template: generate_audit_template(&sink.description),
                    });
                }
            }
            ActionKind::Encryption => {
                for sink in taint.storage_sinks() {
                    changes.push(CodeChange {
                        file: sink.file.clone(),
                        change_type: ChangeType::AddEncryption,
                        description: format!("Encrypt data before storage at line {}", sink.line),
                        code_template: "# Encrypt sensitive fields before storage\nfrom cryptography.fernet import Fernet\nencrypted = fernet.encrypt(data.encode())".to_string(),
                    });
                }
            }
            ActionKind::RetentionPolicy => {
                for sink in taint.storage_sinks() {
                    let ttl = req.timeframe.as_ref().map(|t| t.duration_hours).unwrap_or(8760); // Default 1 year
                    changes.push(CodeChange {
                        file: sink.file.clone(),
                        change_type: ChangeType::AddRetentionPolicy,
                        description: format!("Add {}h retention policy for data at line {}", ttl, sink.line),
                        code_template: format!(
                            "# Set retention policy: auto-delete after {} hours\nretention_ttl = timedelta(hours={})",
                            ttl, ttl
                        ),
                    });
                }
            }
            ActionKind::ConsentCollection => {
                for source in &taint.sources {
                    changes.push(CodeChange {
                        file: source.file.clone(),
                        change_type: ChangeType::AddConsentCheck,
                        description: format!("Add consent verification before data collection at line {}", source.line),
                        code_template: "# Verify user consent before processing\nif not user.has_consent('data_processing'):\n    raise ConsentRequired('Data processing consent required')".to_string(),
                    });
                }
            }
            _ => {} // Other actions handled by LLM
        }

        let priority = match req.severity {
            ComplianceSeverity::Critical => 0,
            ComplianceSeverity::High => 1,
            ComplianceSeverity::Medium => 2,
            ComplianceSeverity::Low => 3,
        };

        if !changes.is_empty() {
            plans.push(ComplianceCodePlan {
                requirement_id: req.id.clone(),
                changes,
                priority,
            });
        }
    }

    plans.sort_by_key(|p| p.priority);
    plans
}

fn generate_deletion_template(sink_kind: &SinkKind, description: &str) -> String {
    match sink_kind {
        SinkKind::DatabaseWrite => format!(
            "def delete_user_data(user_id):\n    \"\"\"Delete all personal data for user. Required by GDPR Art. 17.\"\"\"\n    cursor.execute(\"DELETE FROM {} WHERE user_id = %s\", (user_id,))\n    db.commit()",
            description.chars().take(20).collect::<String>()
        ),
        SinkKind::CacheStore => "def purge_cache(user_id):\n    \"\"\"Purge cached user data.\"\"\"\n    cache.delete_pattern(f\"user:{user_id}:*\")".to_string(),
        SinkKind::FileWrite => "def delete_user_files(user_id):\n    \"\"\"Delete user files from storage.\"\"\"\n    import shutil\n    user_dir = os.path.join(STORAGE_ROOT, str(user_id))\n    if os.path.exists(user_dir):\n        shutil.rmtree(user_dir)".to_string(),
        _ => "# TODO: Implement deletion handler for this storage type".to_string(),
    }
}

fn generate_audit_template(description: &str) -> String {
    format!(
        "# Audit log entry — DO NOT log personal data values\naudit_logger.info(\n    \"data_access\",\n    action=\"{}\",\n    user_id=current_user.id,\n    timestamp=datetime.utcnow().isoformat(),\n    ip_address=request.remote_addr,\n)",
        description.chars().take(40).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regulation::*;
    use crate::taint::*;

    #[test]
    fn test_generate_deletion_handlers() {
        let requirements = vec![ComplianceRequirement {
            id: "gdpr-17".into(), regulation: RegulationKind::GDPR,
            article: "Article 17".into(), section: None,
            title: "Right to erasure".into(),
            description: "Data subject right to deletion".into(),
            data_scope: DataScope::PersonalData,
            action_required: ActionKind::DataDeletion,
            conditions: vec![], timeframe: Some(Timeframe { duration_hours: 720, description: "30 days".into() }),
            cross_references: vec![], severity: ComplianceSeverity::Critical,
        }];

        let code = "data = request.json\ndb.insert({'email': data['email']})\ncache.set('user', data)";
        let taint = TaintAnalysis::analyze("app.py", code);
        let plans = generate_compliance_code(&requirements, &taint);

        assert!(!plans.is_empty(), "Should generate deletion handlers");
        let total_changes: usize = plans.iter().map(|p| p.changes.len()).sum();
        assert!(total_changes >= 1, "Should have at least 1 deletion handler");
    }
}
