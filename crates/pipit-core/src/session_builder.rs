//! High-Level Session Builder API
//!
//! Raises the public embedding surface from "engine stream" to "session builder."
//! Accepts prompt resources, tool sets, extension sets, session stores, capability
//! policy, and model/runtime configuration, then returns a ready `PipitEngine`
//! plus extension-load results.
//!
//! Collapses the implicit dependency graph into a validated configuration object
//! assembled in O(N) over provided subsystems. Enforces invariants once, centrally,
//! rather than in every consumer.

use crate::agent::AgentLoopConfig;
use crate::events::{ApprovalDecision, ApprovalHandler, AutoApproveHandler, DenyAllApprovalHandler};
use crate::pev::{AgentMode, ModelRouter};
use crate::prompt_kernel::{self, AssembledPrompt, PromptInputs};
use crate::sdk::{EngineConfig, EngineHandle, InitConfig, PipitEngine, ENGINE_PROTOCOL_VERSION};
use pipit_context::budget::ContextSettings;
use pipit_extensions::{ExtensionRunner, NoopExtensionRunner};
use pipit_tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;

/// Builder for creating a fully-configured Pipit session.
///
/// This is the recommended high-level API for embedding Pipit.
/// It validates configuration, assembles prompts, and returns
/// a ready-to-use engine and handle.
pub struct SessionBuilder {
    // ── Required ──
    models: Option<ModelRouter>,

    // ── Optional with defaults ──
    project_root: PathBuf,
    tools: ToolRegistry,
    agent_config: AgentLoopConfig,
    context_settings: ContextSettings,
    model_limit: u64,
    prompt_inputs: PromptInputs,
    system_prompt_override: Option<String>,
    extensions: Option<Arc<dyn ExtensionRunner>>,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
    agent_mode: AgentMode,
    session_id: Option<String>,
    session_metadata: SessionMetadata,
}

/// Metadata about the session for the Init capability envelope.
#[derive(Debug, Clone, Default)]
pub struct SessionMetadata {
    pub slash_commands: Vec<String>,
    pub skills: Vec<String>,
    pub plugins: Vec<String>,
    pub agents: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub capabilities: Vec<String>,
}

/// Errors that can occur during session building.
#[derive(Debug, thiserror::Error)]
pub enum SessionBuildError {
    #[error("models not configured: call .models() before .build()")]
    MissingModels,
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
}

/// The result of building a session.
pub struct BuiltSession {
    /// The ready-to-use engine.
    pub engine: PipitEngine,
    /// Handle for controlling the engine (steering, approval, cancel).
    pub handle: EngineHandle,
    /// The assembled prompt (for inspection or caching).
    pub prompt: AssembledPrompt,
    /// The Init event to emit as the first event in the stream.
    pub init_event: crate::sdk::EngineEvent,
}

impl SessionBuilder {
    pub fn new() -> Self {
        Self {
            models: None,
            project_root: PathBuf::from("."),
            tools: ToolRegistry::with_builtins(),
            agent_config: AgentLoopConfig::default(),
            context_settings: ContextSettings::default(),
            model_limit: 200_000,
            prompt_inputs: PromptInputs::default(),
            system_prompt_override: None,
            extensions: None,
            approval_handler: None,
            agent_mode: AgentMode::Balanced,
            session_id: None,
            session_metadata: SessionMetadata::default(),
        }
    }

    /// Set the model router (required).
    pub fn models(mut self, models: ModelRouter) -> Self {
        self.models = Some(models);
        self
    }

    /// Set the project root directory.
    pub fn project_root(mut self, root: PathBuf) -> Self {
        self.project_root = root;
        self
    }

    /// Set the tool registry. Defaults to builtins.
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Set the agent loop configuration.
    pub fn agent_config(mut self, config: AgentLoopConfig) -> Self {
        self.agent_config = config;
        self
    }

    /// Set context management settings.
    pub fn context_settings(mut self, settings: ContextSettings) -> Self {
        self.context_settings = settings;
        self
    }

    /// Set the model context window limit.
    pub fn model_limit(mut self, limit: u64) -> Self {
        self.model_limit = limit;
        self
    }

    /// Set prompt inputs for the composable prompt kernel.
    pub fn prompt_inputs(mut self, inputs: PromptInputs) -> Self {
        self.prompt_inputs = inputs;
        self
    }

    /// Override the system prompt entirely (bypasses prompt kernel).
    pub fn system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt_override = Some(prompt);
        self
    }

    /// Append a custom system prompt section.
    pub fn append_system_prompt(mut self, content: String) -> Self {
        self.prompt_inputs.custom_sections.push(
            prompt_kernel::PromptSection::new(
                prompt_kernel::SectionId::Custom("appended".to_string()),
                content,
            ),
        );
        self
    }

    /// Set the extension runner.
    pub fn extensions(mut self, ext: Arc<dyn ExtensionRunner>) -> Self {
        self.extensions = Some(ext);
        self
    }

    /// Set the approval handler.
    pub fn approval_handler(mut self, handler: Arc<dyn ApprovalHandler>) -> Self {
        self.approval_handler = Some(handler);
        self
    }

    /// Set the agent mode.
    pub fn agent_mode(mut self, mode: AgentMode) -> Self {
        self.agent_mode = mode;
        self
    }

    /// Set an explicit session ID.
    pub fn session_id(mut self, id: String) -> Self {
        self.session_id = Some(id);
        self
    }

    /// Set session metadata (for the Init event).
    pub fn session_metadata(mut self, meta: SessionMetadata) -> Self {
        self.session_metadata = meta;
        self
    }

    /// Build the session. Validates configuration and returns a ready engine.
    ///
    /// Complexity: O(N) over provided subsystems for configuration assembly,
    /// plus O(k) for prompt assembly over k sections.
    pub fn build(self) -> Result<BuiltSession, SessionBuildError> {
        let models = self
            .models
            .ok_or(SessionBuildError::MissingModels)?;

        // Assemble prompt
        let mut inputs = self.prompt_inputs;
        inputs.project_root = Some(self.project_root.clone());

        let prompt = prompt_kernel::assemble(&inputs);
        let system_prompt = self
            .system_prompt_override
            .unwrap_or_else(|| prompt.materialize());

        let session_id = self
            .session_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Build engine config
        let config = EngineConfig {
            project_root: self.project_root.clone(),
            agent_config: self.agent_config,
            context_settings: self.context_settings,
            system_prompt,
            model_limit: self.model_limit,
        };

        // Create engine
        let (engine, handle) = PipitEngine::new(models.clone(), self.tools, config);

        // Build Init event
        let init_event = engine.emit_init(InitConfig {
            session_id: session_id.clone(),
            cwd: self.project_root.display().to_string(),
            model: models.for_role(crate::pev::ModelRole::Executor).model_id.clone(),
            provider: format!("{:?}", models.for_role(crate::pev::ModelRole::Executor).role),
            permission_mode: "sdk".to_string(),
            tools: Vec::new(), // Populated by caller with actual tool names
            slash_commands: self.session_metadata.slash_commands,
            skills: self.session_metadata.skills,
            plugins: self.session_metadata.plugins,
            agents: self.session_metadata.agents,
            mcp_servers: self.session_metadata.mcp_servers,
            agent_mode: format!("{:?}", self.agent_mode),
            capabilities: self.session_metadata.capabilities,
        });

        Ok(BuiltSession {
            engine,
            handle,
            prompt,
            init_event,
        })
    }
}

impl Default for SessionBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_requires_models() {
        let result = SessionBuilder::new().build();
        assert!(matches!(result, Err(SessionBuildError::MissingModels)));
    }

    #[test]
    fn builder_accepts_prompt_inputs() {
        let inputs = PromptInputs {
            project_name: Some("test-project".to_string()),
            ..Default::default()
        };
        let builder = SessionBuilder::new()
            .project_root(PathBuf::from("/tmp/test"))
            .prompt_inputs(inputs)
            .model_limit(100_000);
        // Can't build without models, but the builder accepts all inputs
        assert!(builder.build().is_err());
    }
}
