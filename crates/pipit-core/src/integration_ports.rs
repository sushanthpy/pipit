//! Integration Ports — ForgePort, MessagingPort, TeamPort
//!
//! Extends the kernel's port boundary with forge-level (GitHub/GitLab),
//! messaging (Slack/Discord), and team management interfaces.
//! Each is a trait that adapters implement — zero coupling to specific platforms.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════
//  ForgePort — Git Forge Integration (GitHub, GitLab, Bitbucket)
// ═══════════════════════════════════════════════════════════════════════

/// Pull request specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrSpec {
    pub title: String,
    pub body: String,
    pub head_branch: String,
    pub base_branch: String,
    pub draft: bool,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
}

/// Handle to a created pull request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrHandle {
    pub number: u64,
    pub url: String,
    pub html_url: String,
    pub state: String,
}

/// A review comment on a PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: u64,
    pub body: String,
    pub path: String,
    pub line: Option<u32>,
    pub author: String,
    pub created_at: String,
}

/// Spec for posting a review comment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewCommentSpec {
    pub body: String,
    pub path: String,
    pub line: Option<u32>,
    pub side: Option<String>,
}

/// CI/CD status for a commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiStatus {
    pub state: CiState,
    pub checks: Vec<CiCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiState { Pending, Success, Failure, Error }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiCheck {
    pub name: String,
    pub status: CiState,
    pub url: Option<String>,
}

/// Issue specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSpec {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
}

/// Handle to a created issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueHandle {
    pub number: u64,
    pub url: String,
}

/// Installation token for GitHub App.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallationToken {
    pub token: String,
    pub expires_at: String,
    pub permissions: std::collections::HashMap<String, String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    #[error("Auth error: {0}")]
    Auth(String),
    #[error("Rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("API error: {0}")]
    Api(String),
    #[error("Network error: {0}")]
    Network(String),
}

/// Forge integration port — abstraction over GitHub/GitLab/Bitbucket.
#[async_trait]
pub trait ForgePort: Send + Sync {
    /// Get the forge name (e.g., "github", "gitlab").
    fn name(&self) -> &str;

    /// Create a pull request.
    async fn create_pull_request(&self, spec: PrSpec) -> Result<PrHandle, ForgeError>;

    /// List review comments on a PR.
    async fn list_review_comments(&self, pr: &PrHandle) -> Result<Vec<ReviewComment>, ForgeError>;

    /// Post a review comment on a PR.
    async fn post_review_comment(&self, pr: &PrHandle, comment: ReviewCommentSpec) -> Result<(), ForgeError>;

    /// Get CI status for a commit SHA.
    async fn ci_status(&self, commit_sha: &str) -> Result<CiStatus, ForgeError>;

    /// Create an issue.
    async fn create_issue(&self, spec: IssueSpec) -> Result<IssueHandle, ForgeError>;

    /// Install/authenticate a forge app (e.g., GitHub App OAuth device flow).
    async fn install_app(&self, org: &str) -> Result<InstallationToken, ForgeError>;

    /// Check rate limit status.
    async fn rate_limit_status(&self) -> Result<RateLimitInfo, ForgeError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitInfo {
    pub remaining: u32,
    pub limit: u32,
    pub reset_at: u64,
}

/// No-op forge port for environments without forge integration.
pub struct NullForgePort;

#[async_trait]
impl ForgePort for NullForgePort {
    fn name(&self) -> &str { "none" }
    async fn create_pull_request(&self, _: PrSpec) -> Result<PrHandle, ForgeError> {
        Err(ForgeError::Api("No forge configured".into()))
    }
    async fn list_review_comments(&self, _: &PrHandle) -> Result<Vec<ReviewComment>, ForgeError> { Ok(vec![]) }
    async fn post_review_comment(&self, _: &PrHandle, _: ReviewCommentSpec) -> Result<(), ForgeError> { Ok(()) }
    async fn ci_status(&self, _: &str) -> Result<CiStatus, ForgeError> {
        Ok(CiStatus { state: CiState::Pending, checks: vec![] })
    }
    async fn create_issue(&self, _: IssueSpec) -> Result<IssueHandle, ForgeError> {
        Err(ForgeError::Api("No forge configured".into()))
    }
    async fn install_app(&self, _: &str) -> Result<InstallationToken, ForgeError> {
        Err(ForgeError::Api("No forge configured".into()))
    }
    async fn rate_limit_status(&self) -> Result<RateLimitInfo, ForgeError> {
        Ok(RateLimitInfo { remaining: 0, limit: 0, reset_at: 0 })
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  MessagingPort — Slack, Discord, Webhooks
// ═══════════════════════════════════════════════════════════════════════

/// A message to send via a messaging platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub channel: String,
    pub text: String,
    pub thread_id: Option<String>,
    pub attachments: Vec<MessageAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageAttachment {
    pub title: String,
    pub text: String,
    pub color: Option<String>,
}

/// A received message from a messaging platform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub channel: String,
    pub text: String,
    pub thread_id: Option<String>,
    pub author: String,
    pub platform: String,
    pub timestamp: String,
}

#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("Auth error: {0}")]
    Auth(String),
    #[error("Rate limited")]
    RateLimited,
    #[error("Channel not found: {0}")]
    ChannelNotFound(String),
    #[error("Send failed: {0}")]
    SendFailed(String),
}

/// Messaging platform port — Slack, Discord, Telegram, webhooks.
#[async_trait]
pub trait MessagingPort: Send + Sync {
    /// Platform name.
    fn platform(&self) -> &str;

    /// Send a message.
    async fn send(&self, msg: OutboundMessage) -> Result<String, MessagingError>;

    /// Send a notification (non-threaded).
    async fn notify(&self, channel: &str, text: &str) -> Result<(), MessagingError>;

    /// Install/authorize the messaging app (OAuth flow).
    async fn install(&self, workspace: &str) -> Result<String, MessagingError>;

    /// Map a pipit session ID to a messaging thread.
    async fn map_session(&self, session_id: &str, thread_id: &str) -> Result<(), MessagingError>;
}

/// No-op messaging port.
pub struct NullMessagingPort;

#[async_trait]
impl MessagingPort for NullMessagingPort {
    fn platform(&self) -> &str { "none" }
    async fn send(&self, _: OutboundMessage) -> Result<String, MessagingError> { Ok(String::new()) }
    async fn notify(&self, _: &str, _: &str) -> Result<(), MessagingError> { Ok(()) }
    async fn install(&self, _: &str) -> Result<String, MessagingError> {
        Err(MessagingError::Auth("No messaging configured".into()))
    }
    async fn map_session(&self, _: &str, _: &str) -> Result<(), MessagingError> { Ok(()) }
}

// ═══════════════════════════════════════════════════════════════════════
//  TeamPort — Organization & Team Management
// ═══════════════════════════════════════════════════════════════════════

/// Team definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub members: Vec<TeamMember>,
    pub settings: TeamSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub user_id: String,
    pub role: TeamRole,
    pub joined_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeamRole {
    Admin,
    Developer,
    Viewer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamSettings {
    /// Allowed LLM providers for this team.
    pub allowed_providers: Vec<String>,
    /// Cost budget per session (USD).
    pub cost_budget: Option<f64>,
    /// Shared skill IDs available to team members.
    pub shared_skills: Vec<String>,
    /// Path restrictions (team members can only edit within these paths).
    pub path_restrictions: Vec<String>,
    /// Capability set restriction (bitset, intersected with user capabilities).
    pub capability_mask: Option<u32>,
}

impl Default for TeamSettings {
    fn default() -> Self {
        Self {
            allowed_providers: vec![],
            cost_budget: None,
            shared_skills: vec![],
            path_restrictions: vec![],
            capability_mask: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TeamError {
    #[error("Team not found: {0}")]
    NotFound(String),
    #[error("Permission denied: {0}")]
    PermissionDenied(String),
    #[error("Already exists: {0}")]
    AlreadyExists(String),
    #[error("Storage error: {0}")]
    Storage(String),
}

/// Team management port.
#[async_trait]
pub trait TeamPort: Send + Sync {
    /// Create a new team.
    async fn create_team(&self, name: &str, creator: &str) -> Result<Team, TeamError>;
    /// Delete a team.
    async fn delete_team(&self, team_id: &str) -> Result<(), TeamError>;
    /// Add a member to a team.
    async fn add_member(&self, team_id: &str, user_id: &str, role: TeamRole) -> Result<(), TeamError>;
    /// Remove a member from a team.
    async fn remove_member(&self, team_id: &str, user_id: &str) -> Result<(), TeamError>;
    /// Get a team by ID.
    async fn get_team(&self, team_id: &str) -> Result<Team, TeamError>;
    /// List all teams for a user.
    async fn list_teams(&self, user_id: &str) -> Result<Vec<Team>, TeamError>;
    /// Update team settings.
    async fn update_settings(&self, team_id: &str, settings: TeamSettings) -> Result<(), TeamError>;
    /// Evaluate effective capabilities for a user in a team context.
    fn effective_capabilities(&self, user_caps: u32, team: &Team) -> u32 {
        match team.settings.capability_mask {
            Some(mask) => user_caps & mask,
            None => user_caps,
        }
    }
}

/// No-op team port for single-user mode.
pub struct NullTeamPort;

#[async_trait]
impl TeamPort for NullTeamPort {
    async fn create_team(&self, _: &str, _: &str) -> Result<Team, TeamError> {
        Err(TeamError::PermissionDenied("Teams not enabled".into()))
    }
    async fn delete_team(&self, _: &str) -> Result<(), TeamError> { Ok(()) }
    async fn add_member(&self, _: &str, _: &str, _: TeamRole) -> Result<(), TeamError> { Ok(()) }
    async fn remove_member(&self, _: &str, _: &str) -> Result<(), TeamError> { Ok(()) }
    async fn get_team(&self, _: &str) -> Result<Team, TeamError> {
        Err(TeamError::NotFound("Teams not enabled".into()))
    }
    async fn list_teams(&self, _: &str) -> Result<Vec<Team>, TeamError> { Ok(vec![]) }
    async fn update_settings(&self, _: &str, _: TeamSettings) -> Result<(), TeamError> { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn team_capability_masking() {
        let port = NullTeamPort;
        let team = Team {
            id: "t1".into(),
            name: "Backend".into(),
            members: vec![],
            settings: TeamSettings {
                capability_mask: Some(0b0000_0011), // only FsRead + FsWrite
                ..Default::default()
            },
        };
        let user_caps = 0b1111_1111; // all caps
        let effective = port.effective_capabilities(user_caps, &team);
        assert_eq!(effective, 0b0000_0011);
    }

    #[test]
    fn team_no_mask_preserves_caps() {
        let port = NullTeamPort;
        let team = Team {
            id: "t1".into(),
            name: "All".into(),
            members: vec![],
            settings: TeamSettings::default(),
        };
        let user_caps = 0xFF;
        assert_eq!(port.effective_capabilities(user_caps, &team), 0xFF);
    }
}
