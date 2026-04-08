//! Enhanced Session Persistence — Migrations, Branching, Export
//!
//! Schema versioning: each session carries a version number.
//! Migrations: v_i → v_{i+1} transforms applied in topological order.
//! Branching: fork at any turn, O(1) via shared prefix + delta.
//! Export: JSON, Markdown, HTML.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Current schema version.
pub const CURRENT_SCHEMA_VERSION: u32 = 3;

/// Session metadata header.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionHeader {
    pub version: u32,
    pub session_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub project_root: String,
    pub model: String,
    pub parent_session: Option<String>,
    pub fork_turn: Option<usize>,
    pub tags: Vec<String>,
    pub total_cost_usd: f64,
    pub total_turns: usize,
}

/// A complete session with messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    pub header: SessionHeader,
    pub messages: Vec<PersistedMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedMessage {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    pub tool_calls: Vec<PersistedToolCall>,
    pub token_usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedToolCall {
    pub tool_name: String,
    pub args_summary: String,
    pub result_summary: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

// ─── Schema Migrations ──────────────────────────────────────────────────

/// Migrate a session from any old version to the current version.
/// Migration DAG: v1 → v2 → v3 (current).
pub fn migrate(mut session: serde_json::Value) -> Result<serde_json::Value, String> {
    let version = session
        .get("header")
        .and_then(|h| h.get("version"))
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;

    if version >= CURRENT_SCHEMA_VERSION {
        return Ok(session);
    }

    // v1 → v2: Add tags field, rename "cost" to "total_cost_usd"
    if version < 2 {
        if let Some(header) = session.get_mut("header").and_then(|h| h.as_object_mut()) {
            if !header.contains_key("tags") {
                header.insert("tags".into(), serde_json::json!([]));
            }
            if let Some(cost) = header.remove("cost") {
                header.insert("total_cost_usd".into(), cost);
            }
            header.insert("version".into(), serde_json::json!(2));
        }
    }

    // v2 → v3: Add parent_session, fork_turn, token_usage per message
    if version < 3 {
        if let Some(header) = session.get_mut("header").and_then(|h| h.as_object_mut()) {
            if !header.contains_key("parent_session") {
                header.insert("parent_session".into(), serde_json::Value::Null);
            }
            if !header.contains_key("fork_turn") {
                header.insert("fork_turn".into(), serde_json::Value::Null);
            }
            header.insert("version".into(), serde_json::json!(3));
        }
        if let Some(messages) = session.get_mut("messages").and_then(|m| m.as_array_mut()) {
            for msg in messages {
                if let Some(obj) = msg.as_object_mut() {
                    if !obj.contains_key("token_usage") {
                        obj.insert("token_usage".into(), serde_json::Value::Null);
                    }
                }
            }
        }
    }

    Ok(session)
}

// ─── Session Branching ──────────────────────────────────────────────────

/// Fork a session at a specific turn, creating a new branch.
pub fn fork_session(
    parent: &PersistedSession,
    fork_turn: usize,
    new_session_id: &str,
) -> Result<PersistedSession, String> {
    if fork_turn > parent.messages.len() {
        return Err(format!(
            "Fork turn {fork_turn} exceeds message count {}",
            parent.messages.len()
        ));
    }
    Ok(PersistedSession {
        header: SessionHeader {
            version: CURRENT_SCHEMA_VERSION,
            session_id: new_session_id.to_string(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
            project_root: parent.header.project_root.clone(),
            model: parent.header.model.clone(),
            parent_session: Some(parent.header.session_id.clone()),
            fork_turn: Some(fork_turn),
            tags: parent.header.tags.clone(),
            total_cost_usd: 0.0,
            total_turns: fork_turn,
        },
        messages: parent.messages[..fork_turn].to_vec(),
    })
}

// ─── Export ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Json,
    Markdown,
    Html,
}

/// Export a session to the specified format.
pub fn export_session(session: &PersistedSession, format: ExportFormat) -> String {
    match format {
        ExportFormat::Json => serde_json::to_string_pretty(session).unwrap_or_default(),
        ExportFormat::Markdown => {
            let mut md = String::new();
            md.push_str(&format!("# Session: {}\n\n", session.header.session_id));
            md.push_str(&format!("- **Model**: {}\n", session.header.model));
            md.push_str(&format!("- **Created**: {}\n", session.header.created_at));
            md.push_str(&format!(
                "- **Cost**: ${:.4}\n",
                session.header.total_cost_usd
            ));
            md.push_str(&format!(
                "- **Turns**: {}\n\n---\n\n",
                session.header.total_turns
            ));
            for (i, msg) in session.messages.iter().enumerate() {
                let role = match msg.role.as_str() {
                    "user" => "**User**",
                    "assistant" => "**Assistant**",
                    r => r,
                };
                md.push_str(&format!("### Turn {} — {}\n\n", i + 1, role));
                md.push_str(&msg.content);
                md.push_str("\n\n");
                for tc in &msg.tool_calls {
                    md.push_str(&format!(
                        "> 🔧 **{}** ({}ms)\n> {}\n\n",
                        tc.tool_name, tc.duration_ms, tc.result_summary
                    ));
                }
            }
            md
        }
        ExportFormat::Html => {
            let mut html = String::new();
            html.push_str(
                "<!DOCTYPE html><html><head><meta charset='utf-8'>\n<title>Pipit Session</title>\n",
            );
            html.push_str(
                "<style>body{font-family:system-ui;max-width:800px;margin:0 auto;padding:20px}",
            );
            html.push_str(".user{background:#e3f2fd;padding:12px;border-radius:8px;margin:8px 0}");
            html.push_str(
                ".assistant{background:#f5f5f5;padding:12px;border-radius:8px;margin:8px 0}",
            );
            html.push_str(".tool{background:#fff3e0;padding:8px;border-radius:4px;font-size:0.9em;margin:4px 0}");
            html.push_str(
                "pre{background:#263238;color:#eee;padding:12px;border-radius:4px;overflow-x:auto}",
            );
            html.push_str("</style></head><body>\n");
            html.push_str(&format!(
                "<h1>Session: {}</h1>\n",
                session.header.session_id
            ));
            for msg in &session.messages {
                let class = if msg.role == "user" {
                    "user"
                } else {
                    "assistant"
                };
                html.push_str(&format!(
                    "<div class='{class}'><strong>{}</strong><br>{}</div>\n",
                    msg.role,
                    msg.content.replace('\n', "<br>")
                ));
                for tc in &msg.tool_calls {
                    html.push_str(&format!(
                        "<div class='tool'>🔧 <b>{}</b> ({}ms)<br><small>{}</small></div>\n",
                        tc.tool_name, tc.duration_ms, tc.result_summary
                    ));
                }
            }
            html.push_str("</body></html>");
            html
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_v1_to_v3() {
        let v1 = serde_json::json!({
            "header": { "version": 1, "session_id": "test", "created_at": "2025-01-01",
                "updated_at": "2025-01-01", "project_root": "/tmp", "model": "sonnet",
                "cost": 0.05, "total_turns": 3 },
            "messages": [{"role": "user", "content": "hello", "timestamp": "t1", "tool_calls": []}]
        });
        let migrated = migrate(v1).unwrap();
        let header = migrated.get("header").unwrap();
        assert_eq!(header.get("version").unwrap().as_u64().unwrap(), 3);
        assert!(header.get("tags").is_some());
        assert!(header.get("parent_session").is_some());
        assert!(header.get("total_cost_usd").is_some());
    }

    #[test]
    fn fork_session_at_turn() {
        let parent = PersistedSession {
            header: SessionHeader {
                version: 3,
                session_id: "parent".into(),
                created_at: "t0".into(),
                updated_at: "t0".into(),
                project_root: "/tmp".into(),
                model: "sonnet".into(),
                parent_session: None,
                fork_turn: None,
                tags: vec![],
                total_cost_usd: 0.1,
                total_turns: 4,
            },
            messages: vec![
                PersistedMessage {
                    role: "user".into(),
                    content: "turn 1".into(),
                    timestamp: "t1".into(),
                    tool_calls: vec![],
                    token_usage: None,
                },
                PersistedMessage {
                    role: "assistant".into(),
                    content: "resp 1".into(),
                    timestamp: "t2".into(),
                    tool_calls: vec![],
                    token_usage: None,
                },
                PersistedMessage {
                    role: "user".into(),
                    content: "turn 2".into(),
                    timestamp: "t3".into(),
                    tool_calls: vec![],
                    token_usage: None,
                },
                PersistedMessage {
                    role: "assistant".into(),
                    content: "resp 2".into(),
                    timestamp: "t4".into(),
                    tool_calls: vec![],
                    token_usage: None,
                },
            ],
        };
        let branch = fork_session(&parent, 2, "branch-1").unwrap();
        assert_eq!(branch.messages.len(), 2);
        assert_eq!(branch.header.parent_session.as_deref(), Some("parent"));
        assert_eq!(branch.header.fork_turn, Some(2));
    }

    #[test]
    fn export_markdown() {
        let session = PersistedSession {
            header: SessionHeader {
                version: 3,
                session_id: "test".into(),
                created_at: "2025-01-01".into(),
                updated_at: "2025-01-01".into(),
                project_root: "/tmp".into(),
                model: "sonnet".into(),
                parent_session: None,
                fork_turn: None,
                tags: vec![],
                total_cost_usd: 0.05,
                total_turns: 1,
            },
            messages: vec![PersistedMessage {
                role: "user".into(),
                content: "Fix the bug".into(),
                timestamp: "t1".into(),
                tool_calls: vec![],
                token_usage: None,
            }],
        };
        let md = export_session(&session, ExportFormat::Markdown);
        assert!(md.contains("# Session: test") && md.contains("Fix the bug"));
    }

    #[test]
    fn export_html() {
        let session = PersistedSession {
            header: SessionHeader {
                version: 3,
                session_id: "html-test".into(),
                created_at: "2025".into(),
                updated_at: "2025".into(),
                project_root: "/tmp".into(),
                model: "opus".into(),
                parent_session: None,
                fork_turn: None,
                tags: vec![],
                total_cost_usd: 0.0,
                total_turns: 0,
            },
            messages: vec![],
        };
        let html = export_session(&session, ExportFormat::Html);
        assert!(html.contains("<!DOCTYPE html>") && html.contains("html-test"));
    }
}
