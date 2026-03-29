//! A2A (Agent-to-Agent) Protocol — Google's interoperability standard.
//!
//! Enables pipit to communicate with agents built on other frameworks
//! (CrewAI, LangGraph, AutoGen) via a shared task protocol.
//!
//! Discovery: /.well-known/agent.json → AgentCard
//! Task protocol: tasks/send → status polling → result
//! Transport: HTTP + SSE

use serde::{Deserialize, Serialize};

/// A2A Agent Card — advertises an agent's capabilities.
/// Served at `/.well-known/agent.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub version: String,
    pub capabilities: AgentCapabilities,
    pub skills: Vec<AgentSkill>,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    pub streaming: bool,
    pub push_notifications: bool,
    pub state_transition_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
}

/// A2A Task — sent via `tasks/send`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2ATask {
    pub id: String,
    pub session_id: Option<String>,
    pub message: A2AMessage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    pub role: String,
    pub parts: Vec<A2APart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum A2APart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "file")]
    File { name: String, data: String, mime_type: String },
}

/// A2A Task status response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2ATaskStatus {
    pub id: String,
    pub status: TaskState,
    pub message: Option<A2AMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskState {
    pub state: String, // submitted, working, input-required, completed, failed, canceled
    pub message: Option<String>,
}

/// Build the pipit Agent Card for A2A discovery.
pub fn pipit_agent_card(base_url: &str) -> AgentCard {
    AgentCard {
        name: "pipit".to_string(),
        description: "AI coding agent with terminal-native tool execution, code intelligence, and distributed mesh support".to_string(),
        url: base_url.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: AgentCapabilities {
            streaming: true,
            push_notifications: false,
            state_transition_history: true,
        },
        skills: vec![
            AgentSkill {
                id: "code-edit".to_string(),
                name: "Code Editing".to_string(),
                description: "Edit source files with search-replace precision".to_string(),
                tags: vec!["coding".to_string(), "editing".to_string()],
            },
            AgentSkill {
                id: "bash-exec".to_string(),
                name: "Shell Execution".to_string(),
                description: "Execute shell commands with sandboxing".to_string(),
                tags: vec!["terminal".to_string(), "devops".to_string()],
            },
            AgentSkill {
                id: "code-review".to_string(),
                name: "Code Review".to_string(),
                description: "Review uncommitted changes for bugs and style issues".to_string(),
                tags: vec!["review".to_string(), "quality".to_string()],
            },
            AgentSkill {
                id: "bug-fix".to_string(),
                name: "Bug Fixing".to_string(),
                description: "Diagnose and fix bugs from error messages or test failures".to_string(),
                tags: vec!["debugging".to_string(), "testing".to_string()],
            },
        ],
        default_input_modes: vec!["text".to_string()],
        default_output_modes: vec!["text".to_string()],
    }
}

/// Discover a remote A2A agent by fetching its Agent Card.
pub async fn discover_agent(base_url: &str) -> Result<AgentCard, String> {
    let url = format!("{}/.well-known/agent.json", base_url.trim_end_matches('/'));
    let resp = reqwest::get(&url).await
        .map_err(|e| format!("A2A discovery failed: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("A2A agent returned {}", resp.status()));
    }
    resp.json::<AgentCard>().await
        .map_err(|e| format!("Invalid Agent Card: {}", e))
}

/// Send a task to a remote A2A agent.
pub async fn send_task(agent_url: &str, task: &A2ATask) -> Result<A2ATaskStatus, String> {
    let url = format!("{}/tasks/send", agent_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client.post(&url)
        .json(task)
        .send()
        .await
        .map_err(|e| format!("A2A task send failed: {}", e))?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("A2A error: {}", body));
    }
    resp.json::<A2ATaskStatus>().await
        .map_err(|e| format!("Invalid A2A response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_card_serialization() {
        let card = pipit_agent_card("http://localhost:3141");
        let json = serde_json::to_string_pretty(&card).unwrap();
        assert!(json.contains("pipit"));
        assert!(json.contains("code-edit"));
    }
}
