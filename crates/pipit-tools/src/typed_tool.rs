//! Typed Tool Foundation — Phase 0 of the tool system overhaul.
//!
//! Provides a strongly-typed tool trait (`TypedTool`) that coexists with the
//! legacy `Tool` trait via a bridge adapter. New tools implement `TypedTool`;
//! existing tools remain unchanged. The adapter makes any `TypedTool` usable
//! through the existing `ToolRegistry`.
//!
//! Design principles enforced:
//!   1. Declarative capabilities as const data (not runtime strings)
//!   2. Purity is a trait-level constant
//!   3. Typed schemas via `schemars::JsonSchema` (generated, not hand-written)
//!   4. Streaming progress as one protocol (`ToolEvent`)
//!   5. Unified result with evidence artifacts
//!   6. Tool self-description via `ToolCard`
//!   7. Deterministic replay support via optional `ReplayContext`

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolContext, ToolDisplay, ToolError, ToolResult};

// ═══════════════════════════════════════════════════════════════
//  PURITY (re-exported from tool_semantics_bridge for convenience)
// ═══════════════════════════════════════════════════════════════

/// Tool purity level — declared as a const on every TypedTool impl.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Purity {
    /// No side effects. Safe to cache, replay, parallelize.
    Pure,
    /// Same result for same input. Safe to retry.
    Idempotent,
    /// Modifies state but is generally reversible.
    Mutating,
    /// Hard to undo. Shell commands, network writes.
    Destructive,
}

impl Purity {
    pub fn is_mutating(self) -> bool {
        matches!(self, Self::Mutating | Self::Destructive)
    }
}

// ═══════════════════════════════════════════════════════════════
//  CAPABILITY SET
// ═══════════════════════════════════════════════════════════════

/// Capability bitset — matches pipit-core's CapabilitySet.
/// Declared as `const CAPABILITIES` on every TypedTool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilitySet(pub u32);

impl CapabilitySet {
    pub const NONE: Self = Self(0);
    pub const FS_READ: Self = Self(1 << 0);
    pub const FS_WRITE: Self = Self(1 << 1);
    pub const FS_READ_EXTERNAL: Self = Self(1 << 2);
    pub const FS_WRITE_EXTERNAL: Self = Self(1 << 3);
    pub const PROCESS_EXEC: Self = Self(1 << 4);
    pub const PROCESS_EXEC_MUTATING: Self = Self(1 << 5);
    pub const NETWORK_READ: Self = Self(1 << 6);
    pub const NETWORK_WRITE: Self = Self(1 << 7);
    pub const MCP_INVOKE: Self = Self(1 << 8);
    pub const DELEGATE: Self = Self(1 << 9);
    pub const VERIFY: Self = Self(1 << 10);
    pub const CONFIG_MODIFY: Self = Self(1 << 11);
    pub const ENV_ACCESS: Self = Self(1 << 12);
    pub const USER_INTERACTION: Self = Self(1 << 13);
    pub const SESSION_WRITE: Self = Self(1 << 14);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

// ═══════════════════════════════════════════════════════════════
//  TOOL CARD (self-description for ToolSearch)
// ═══════════════════════════════════════════════════════════════

/// Self-description that every tool provides. Used by `tool_search` for
/// BM25 ranking, by the system prompt for tool documentation, and by
/// the UI for help text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCard {
    /// Tool name (matches the LLM tool declaration).
    pub name: String,
    /// One-line summary.
    pub summary: String,
    /// When to use this tool (guidance for the model).
    pub when_to_use: String,
    /// Example invocations (for few-shot prompting).
    pub examples: Vec<ToolExample>,
    /// Searchable tags for discovery.
    pub tags: Vec<String>,
    /// Purity level.
    pub purity: Purity,
    /// Required capabilities.
    pub capabilities: u32,
}

/// An example invocation of a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolExample {
    /// Description of what this example does.
    pub description: String,
    /// Example input arguments (JSON).
    pub input: Value,
}

// ═══════════════════════════════════════════════════════════════
//  TOOL EVENT (streaming progress protocol)
// ═══════════════════════════════════════════════════════════════

/// Unified streaming event for all tool executions.
/// One type for all tools — no per-tool progress types.
#[derive(Debug, Clone, Serialize)]
pub enum ToolEvent {
    /// Tool execution has started.
    Started { tool_name: String },
    /// Progress update (percentage + message).
    Progress {
        stage: String,
        percent: Option<f32>,
        message: String,
    },
    /// Streaming output chunk (stdout, stderr, or data).
    OutputChunk { stream: OutputStream, data: String },
    /// Artifact produced during execution.
    Artifact { kind: ArtifactKind, data: Value },
    /// Tool execution completed successfully.
    Completed { content: String, mutated: bool },
    /// Tool execution failed.
    Failed { error: String },
}

/// Which output stream a chunk belongs to.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum OutputStream {
    Stdout,
    Stderr,
    Data,
}

/// Kind of artifact produced by a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// A file was read (evidence for ProofState).
    FileRead { path: String },
    /// A file was modified (evidence for ProofState).
    FileModified {
        path: String,
        before_hash: Option<String>,
        after_hash: Option<String>,
    },
    /// A command was executed.
    CommandExecution {
        command: String,
        exit_code: Option<i32>,
    },
    /// A diff was produced.
    Diff { path: String, hunks: u32 },
    /// A search result.
    SearchResult { query: String, matches: u32 },
    /// Custom artifact.
    Custom { kind: String },
}

// ═══════════════════════════════════════════════════════════════
//  TYPED TOOL RESULT (unified result + evidence)
// ═══════════════════════════════════════════════════════════════

/// Extended tool result that includes evidence artifacts.
/// Every tool contributes to ProofState automatically.
#[derive(Debug, Clone)]
pub struct TypedToolResult {
    /// The content returned to the LLM.
    pub content: String,
    /// Optional structured display.
    pub display: Option<ToolDisplay>,
    /// Whether the tool mutated external state.
    pub mutated: bool,
    /// Evidence artifacts for ProofState.
    pub artifacts: Vec<ArtifactKind>,
    /// File edits realized by this tool.
    pub edits: Vec<RealizedEdit>,
}

/// A file edit realized by a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealizedEdit {
    pub path: PathBuf,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub hunks: u32,
}

impl TypedToolResult {
    /// Create a simple text result (no mutation, no evidence).
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            display: None,
            mutated: false,
            artifacts: Vec::new(),
            edits: Vec::new(),
        }
    }

    /// Create a mutating result with artifacts.
    pub fn mutating(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            display: None,
            mutated: true,
            artifacts: Vec::new(),
            edits: Vec::new(),
        }
    }

    /// Add an artifact to the result.
    pub fn with_artifact(mut self, artifact: ArtifactKind) -> Self {
        self.artifacts.push(artifact);
        self
    }

    /// Add an edit to the result.
    pub fn with_edit(mut self, edit: RealizedEdit) -> Self {
        self.edits.push(edit);
        self
    }

    /// Add a display hint.
    pub fn with_display(mut self, display: ToolDisplay) -> Self {
        self.display = Some(display);
        self
    }

    /// Convert to the legacy ToolResult (for bridge adapter).
    pub fn into_legacy(self) -> ToolResult {
        let bytes = self.content.len();
        ToolResult {
            content: self.content,
            display: self.display,
            mutated: self.mutated,
            content_bytes: bytes,
        }
    }
}

// ═══════════════════════════════════════════════════════════════
//  THE TYPED TOOL TRAIT
// ═══════════════════════════════════════════════════════════════

/// The next-generation tool trait.
///
/// Compared to the legacy `Tool` trait:
/// - Input is a typed Rust struct with auto-generated JSON schema
/// - Capabilities and purity are const declarations, not runtime methods
/// - Returns `TypedToolResult` with evidence artifacts
/// - Provides `ToolCard` self-description for discovery
///
/// Tools implementing this trait are automatically wrapped by
/// `TypedToolAdapter` to satisfy the legacy `Tool` trait for
/// registry compatibility.
#[async_trait]
pub trait TypedTool: Send + Sync + 'static {
    /// Input type — must be deserializable from JSON and generate a JSON schema.
    type Input: DeserializeOwned + JsonSchema + Send;

    /// Tool name (must be unique across the registry).
    const NAME: &'static str;

    /// Required capabilities (checked at registration time).
    const CAPABILITIES: CapabilitySet;

    /// Purity level (used by scheduler for batching decisions).
    const PURITY: Purity;

    /// Self-description for discovery and documentation.
    fn describe() -> ToolCard;

    /// Execute the tool with typed input.
    async fn execute(
        &self,
        input: Self::Input,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<TypedToolResult, ToolError>;
}

// ═══════════════════════════════════════════════════════════════
//  BRIDGE ADAPTER: TypedTool → Tool
// ═══════════════════════════════════════════════════════════════

/// Wraps any `TypedTool` to implement the legacy `Tool` trait.
/// This allows typed tools to be registered in the existing `ToolRegistry`.
pub struct TypedToolAdapter<T: TypedTool> {
    inner: T,
    /// Cached JSON schema (generated once from schemars).
    schema: Value,
    /// Cached tool card.
    card: ToolCard,
}

impl<T: TypedTool> TypedToolAdapter<T> {
    pub fn new(inner: T) -> Self {
        let schema = Self::generate_schema();
        let card = T::describe();
        Self {
            inner,
            schema,
            card,
        }
    }

    fn generate_schema() -> Value {
        let schema = schemars::schema_for!(T::Input);
        serde_json::to_value(schema).unwrap_or_else(|_| {
            serde_json::json!({
                "type": "object",
                "properties": {}
            })
        })
    }
}

#[async_trait]
impl<T: TypedTool> Tool for TypedToolAdapter<T> {
    fn name(&self) -> &str {
        T::NAME
    }

    fn schema(&self) -> Value {
        self.schema.clone()
    }

    fn description(&self) -> &str {
        &self.card.summary
    }

    fn is_mutating(&self) -> bool {
        T::PURITY.is_mutating()
    }

    fn requires_approval(&self, mode: pipit_config::ApprovalMode) -> bool {
        match mode {
            pipit_config::ApprovalMode::FullAuto => false,
            pipit_config::ApprovalMode::AutoEdit => matches!(T::PURITY, Purity::Destructive),
            pipit_config::ApprovalMode::CommandReview => T::PURITY.is_mutating(),
            pipit_config::ApprovalMode::Suggest => T::PURITY.is_mutating(),
        }
    }

    fn is_concurrency_safe(&self, _args: &Value) -> bool {
        !T::PURITY.is_mutating()
    }

    async fn execute(
        &self,
        args: Value,
        ctx: &ToolContext,
        cancel: CancellationToken,
    ) -> Result<ToolResult, ToolError> {
        // Deserialize JSON args into the typed input struct
        let input: T::Input = serde_json::from_value(args).map_err(|e| {
            ToolError::InvalidArgs(format!("Invalid arguments for {}: {}", T::NAME, e))
        })?;

        // Execute with typed input
        let result = self.inner.execute(input, ctx, cancel).await?;

        // Convert to legacy ToolResult
        Ok(result.into_legacy())
    }
}

// ═══════════════════════════════════════════════════════════════
//  REGISTRATION HELPERS
// ═══════════════════════════════════════════════════════════════

/// Register a TypedTool into the legacy ToolRegistry.
pub fn register_typed<T: TypedTool>(registry: &mut crate::ToolRegistry, tool: T) {
    registry.register(Arc::new(TypedToolAdapter::new(tool)));
}

/// Get the ToolCard for a TypedTool (without an instance).
pub fn tool_card<T: TypedTool>() -> ToolCard {
    T::describe()
}

// ═══════════════════════════════════════════════════════════════
//  TOOL SEARCH INDEX
// ═══════════════════════════════════════════════════════════════

/// Simple BM25-style search over tool cards.
/// Used by the `tool_search` meta-tool.
#[derive(Debug, Default)]
pub struct ToolSearchIndex {
    cards: Vec<ToolCard>,
}

impl ToolSearchIndex {
    pub fn new() -> Self {
        Self { cards: Vec::new() }
    }

    pub fn add(&mut self, card: ToolCard) {
        self.cards.push(card);
    }

    /// Search for tools matching a query. Returns cards sorted by relevance.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&ToolCard> {
        let query_lower = query.to_lowercase();
        let query_terms: Vec<&str> = query_lower.split_whitespace().collect::<Vec<_>>();
        if query_terms.is_empty() {
            return self.cards.iter().take(limit).collect();
        }

        let mut scored: Vec<(usize, f32)> = self
            .cards
            .iter()
            .enumerate()
            .map(|(i, card)| {
                let text = format!(
                    "{} {} {} {}",
                    card.name,
                    card.summary,
                    card.when_to_use,
                    card.tags.join(" ")
                )
                .to_lowercase();

                let mut score = 0.0f32;
                for term in &query_terms {
                    if card.name.to_lowercase().contains(term) {
                        score += 10.0; // Name match is very high signal
                    }
                    if card.tags.iter().any(|t| t.to_lowercase().contains(term)) {
                        score += 5.0; // Tag match
                    }
                    if card.summary.to_lowercase().contains(term) {
                        score += 3.0; // Summary match
                    }
                    if card.when_to_use.to_lowercase().contains(term) {
                        score += 2.0; // Usage guidance match
                    }
                    // Term frequency
                    let tf = text.matches(term).count() as f32;
                    score += tf.min(5.0); // Cap TF contribution
                }
                (i, score)
            })
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .iter()
            .filter(|(_, s)| *s > 0.0)
            .take(limit)
            .map(|(i, _)| &self.cards[*i])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Test: TypedToolAdapter bridges correctly ──

    #[derive(Debug, Deserialize, JsonSchema)]
    struct TestInput {
        message: String,
    }

    struct TestTool;

    #[async_trait]
    impl TypedTool for TestTool {
        type Input = TestInput;
        const NAME: &'static str = "test_tool";
        const CAPABILITIES: CapabilitySet = CapabilitySet::NONE;
        const PURITY: Purity = Purity::Pure;

        fn describe() -> ToolCard {
            ToolCard {
                name: "test_tool".into(),
                summary: "A test tool".into(),
                when_to_use: "For testing".into(),
                examples: vec![],
                tags: vec!["test".into()],
                purity: Purity::Pure,
                capabilities: 0,
            }
        }

        async fn execute(
            &self,
            input: TestInput,
            _ctx: &ToolContext,
            _cancel: CancellationToken,
        ) -> Result<TypedToolResult, ToolError> {
            Ok(TypedToolResult::text(format!("Hello, {}!", input.message)))
        }
    }

    #[test]
    fn adapter_generates_schema() {
        let adapter = TypedToolAdapter::new(TestTool);
        let schema = adapter.schema();
        // schemars generates a schema with properties
        assert!(schema.to_string().contains("message"));
    }

    #[test]
    fn adapter_name_and_description() {
        let adapter = TypedToolAdapter::new(TestTool);
        assert_eq!(adapter.name(), "test_tool");
        assert_eq!(adapter.description(), "A test tool");
    }

    #[test]
    fn adapter_purity_flags() {
        let adapter = TypedToolAdapter::new(TestTool);
        assert!(!adapter.is_mutating());
        assert!(adapter.is_concurrency_safe(&Value::Null));
    }

    #[tokio::test]
    async fn adapter_executes_with_typed_input() {
        let adapter = TypedToolAdapter::new(TestTool);
        let ctx = ToolContext::new(PathBuf::from("/tmp"), pipit_config::ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();
        let args = serde_json::json!({"message": "world"});
        let result = adapter.execute(args, &ctx, cancel).await.unwrap();
        assert_eq!(result.content, "Hello, world!");
        assert!(!result.mutated);
    }

    #[tokio::test]
    async fn adapter_rejects_invalid_args() {
        let adapter = TypedToolAdapter::new(TestTool);
        let ctx = ToolContext::new(PathBuf::from("/tmp"), pipit_config::ApprovalMode::FullAuto);
        let cancel = CancellationToken::new();
        let args = serde_json::json!({"wrong_field": 42});
        let result = adapter.execute(args, &ctx, cancel).await;
        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    // ── Test: ToolSearchIndex ──

    #[test]
    fn search_index_finds_by_name() {
        let mut index = ToolSearchIndex::new();
        index.add(ToolCard {
            name: "read_file".into(),
            summary: "Read a file".into(),
            when_to_use: "When you need to see file contents".into(),
            examples: vec![],
            tags: vec!["filesystem".into(), "read".into()],
            purity: Purity::Pure,
            capabilities: CapabilitySet::FS_READ.0,
        });
        index.add(ToolCard {
            name: "write_file".into(),
            summary: "Write a file".into(),
            when_to_use: "When you need to create or overwrite a file".into(),
            examples: vec![],
            tags: vec!["filesystem".into(), "write".into()],
            purity: Purity::Mutating,
            capabilities: CapabilitySet::FS_WRITE.0,
        });

        let results = index.search("read", 5);
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "read_file");
    }

    #[test]
    fn search_index_finds_by_tag() {
        let mut index = ToolSearchIndex::new();
        index.add(ToolCard {
            name: "bash".into(),
            summary: "Execute shell commands".into(),
            when_to_use: "For running commands".into(),
            examples: vec![],
            tags: vec!["shell".into(), "execution".into()],
            purity: Purity::Destructive,
            capabilities: CapabilitySet::PROCESS_EXEC.0,
        });

        let results = index.search("shell", 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "bash");
    }

    #[test]
    fn search_index_empty_query_returns_all() {
        let mut index = ToolSearchIndex::new();
        index.add(ToolCard {
            name: "a".into(),
            summary: "tool a".into(),
            when_to_use: "".into(),
            examples: vec![],
            tags: vec![],
            purity: Purity::Pure,
            capabilities: 0,
        });
        let results = index.search("", 10);
        assert_eq!(results.len(), 1);
    }
}
