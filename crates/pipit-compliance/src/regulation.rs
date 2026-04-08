//! Regulation Document Parser — Task 9.1
//!
//! Converts structured legal text into machine-readable ComplianceRequirement
//! objects. Two-stage: structural extraction (regex) → semantic extraction.
//! Regulatory delta: edit_distance(R_old, R_new) for change tracking.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RegulationKind {
    GDPR,
    HIPAA,
    PCIDSS,
    SOC2,
    CCPA,
    Custom,
}

/// A machine-readable compliance requirement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceRequirement {
    pub id: String,
    pub regulation: RegulationKind,
    pub article: String,
    pub section: Option<String>,
    pub title: String,
    pub description: String,
    pub data_scope: DataScope,
    pub action_required: ActionKind,
    pub conditions: Vec<String>,
    pub timeframe: Option<Timeframe>,
    pub cross_references: Vec<String>,
    pub severity: ComplianceSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DataScope {
    PersonalData,
    SensitiveData,
    HealthData,
    FinancialData,
    AllUserData,
    SpecificFields(Vec<String>),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ActionKind {
    DataDeletion,
    DataPortability,
    ConsentCollection,
    AuditLogging,
    Encryption,
    AccessControl,
    DataMinimization,
    BreachNotification,
    RetentionPolicy,
    Anonymization,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeframe {
    pub duration_hours: u64,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComplianceSeverity {
    Critical,
    High,
    Medium,
    Low,
}

/// Parser for regulation documents.
pub struct RegulationParser;

impl RegulationParser {
    /// Parse GDPR-style requirements from structured text.
    pub fn parse_gdpr_articles(text: &str) -> Vec<ComplianceRequirement> {
        let mut requirements = Vec::new();
        let mut current_article = String::new();

        for line in text.lines() {
            let trimmed = line.trim();

            // Detect article headers: "Article 17" or "Art. 17"
            if trimmed.starts_with("Article ") || trimmed.starts_with("Art. ") {
                current_article = trimmed.to_string();
                continue;
            }

            if current_article.is_empty() || trimmed.is_empty() {
                continue;
            }

            // Extract requirements from content
            if let Some(req) =
                Self::extract_requirement(trimmed, &current_article, RegulationKind::GDPR)
            {
                requirements.push(req);
            }
        }

        requirements
    }

    fn extract_requirement(
        text: &str,
        article: &str,
        kind: RegulationKind,
    ) -> Option<ComplianceRequirement> {
        let lower = text.to_lowercase();

        // Detect action keywords
        let action = if lower.contains("erasure")
            || lower.contains("delet")
            || lower.contains("right to be forgotten")
        {
            ActionKind::DataDeletion
        } else if lower.contains("portab")
            || lower.contains("export")
            || lower.contains("machine-readable")
        {
            ActionKind::DataPortability
        } else if lower.contains("consent") {
            ActionKind::ConsentCollection
        } else if lower.contains("log") || lower.contains("audit") || lower.contains("record") {
            ActionKind::AuditLogging
        } else if lower.contains("encrypt") || lower.contains("cipher") {
            ActionKind::Encryption
        } else if lower.contains("access control") || lower.contains("authoriz") {
            ActionKind::AccessControl
        } else if lower.contains("breach") || lower.contains("notif") {
            ActionKind::BreachNotification
        } else if lower.contains("retention") || lower.contains("retain") {
            ActionKind::RetentionPolicy
        } else if lower.contains("anonymi") || lower.contains("pseudonymi") {
            ActionKind::Anonymization
        } else if lower.contains("minimiz") {
            ActionKind::DataMinimization
        } else {
            return None; // Not a recognizable requirement
        };

        // Extract timeframe
        let timeframe = Self::extract_timeframe(&lower);

        // Detect data scope
        let scope = if lower.contains("health") || lower.contains("medical") {
            DataScope::HealthData
        } else if lower.contains("financial") || lower.contains("payment") || lower.contains("card")
        {
            DataScope::FinancialData
        } else if lower.contains("sensitive") || lower.contains("special categor") {
            DataScope::SensitiveData
        } else {
            DataScope::PersonalData
        };

        let severity = match action {
            ActionKind::DataDeletion | ActionKind::BreachNotification | ActionKind::Encryption => {
                ComplianceSeverity::Critical
            }
            ActionKind::ConsentCollection | ActionKind::AccessControl => ComplianceSeverity::High,
            ActionKind::AuditLogging | ActionKind::RetentionPolicy => ComplianceSeverity::Medium,
            _ => ComplianceSeverity::Low,
        };

        // Extract cross-references ("as defined in Section X")
        let cross_refs: Vec<String> = lower
            .split("section ")
            .skip(1)
            .filter_map(|s| {
                s.split(|c: char| !c.is_alphanumeric() && c != '(' && c != ')')
                    .next()
            })
            .map(|s| format!("Section {}", s))
            .collect();

        Some(ComplianceRequirement {
            id: format!(
                "{}-{:?}-{}",
                article.replace(' ', "_"),
                action,
                requirements_id()
            ),
            regulation: kind,
            article: article.to_string(),
            section: None,
            title: format!("{:?}", action),
            description: text.to_string(),
            data_scope: scope,
            action_required: action,
            conditions: Vec::new(),
            timeframe,
            cross_references: cross_refs,
            severity,
        })
    }

    fn extract_timeframe(text: &str) -> Option<Timeframe> {
        // "within 72 hours"
        if let Some(idx) = text.find("within ") {
            let rest = &text[idx + 7..];
            let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(num) = num_str.parse::<u64>() {
                if rest.contains("hour") {
                    return Some(Timeframe {
                        duration_hours: num,
                        description: format!("within {} hours", num),
                    });
                }
                if rest.contains("day") {
                    return Some(Timeframe {
                        duration_hours: num * 24,
                        description: format!("within {} days", num),
                    });
                }
            }
        }
        // "30 days"
        for word in text.split_whitespace() {
            if let Ok(num) = word.parse::<u64>() {
                if text.contains("day") {
                    return Some(Timeframe {
                        duration_hours: num * 24,
                        description: format!("{} days", num),
                    });
                }
            }
        }
        None
    }

    /// Compute regulatory delta between old and new requirement sets.
    pub fn compute_delta(
        old: &[ComplianceRequirement],
        new: &[ComplianceRequirement],
    ) -> RegulationDelta {
        let old_ids: std::collections::HashSet<&str> = old.iter().map(|r| r.id.as_str()).collect();
        let new_ids: std::collections::HashSet<&str> = new.iter().map(|r| r.id.as_str()).collect();

        let added: Vec<_> = new
            .iter()
            .filter(|r| !old_ids.contains(r.id.as_str()))
            .cloned()
            .collect();
        let removed: Vec<_> = old
            .iter()
            .filter(|r| !new_ids.contains(r.id.as_str()))
            .cloned()
            .collect();

        RegulationDelta {
            added_count: added.len(),
            removed_count: removed.len(),
            added,
            removed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegulationDelta {
    pub added_count: usize,
    pub removed_count: usize,
    pub added: Vec<ComplianceRequirement>,
    pub removed: Vec<ComplianceRequirement>,
}

fn requirements_id() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gdpr_erasure() {
        let text = "Article 17\nThe data subject shall have the right to obtain from the controller the erasure of personal data within 30 days.";
        let reqs = RegulationParser::parse_gdpr_articles(text);
        assert!(!reqs.is_empty(), "Should find erasure requirement");
        assert!(matches!(reqs[0].action_required, ActionKind::DataDeletion));
        assert!(reqs[0].timeframe.is_some());
        assert_eq!(reqs[0].timeframe.as_ref().unwrap().duration_hours, 30 * 24);
    }

    #[test]
    fn test_parse_breach_notification() {
        let text = "Article 33\nThe controller shall notify the supervisory authority of a personal data breach within 72 hours.";
        let reqs = RegulationParser::parse_gdpr_articles(text);
        assert!(!reqs.is_empty());
        assert!(matches!(
            reqs[0].action_required,
            ActionKind::BreachNotification
        ));
        assert_eq!(reqs[0].timeframe.as_ref().unwrap().duration_hours, 72);
    }

    #[test]
    fn test_severity_classification() {
        let text = "Article 17\nErasure of personal data is required.\nArticle 30\nRecords of processing activities shall be maintained.";
        let reqs = RegulationParser::parse_gdpr_articles(text);
        let erasure = reqs
            .iter()
            .find(|r| matches!(r.action_required, ActionKind::DataDeletion));
        let audit = reqs
            .iter()
            .find(|r| matches!(r.action_required, ActionKind::AuditLogging));
        assert!(erasure.is_some());
        assert_eq!(erasure.unwrap().severity, ComplianceSeverity::Critical);
        if let Some(a) = audit {
            assert_eq!(a.severity, ComplianceSeverity::Medium);
        }
    }
}
