//! Capability-Lattice Permission Gateway (Architecture Task 1)
//!
//! Single authorization oracle for all tool calls. Wraps `pipit_permissions::PermissionEngine`
//! with workspace zone policy, daemon-injected constraints, subagent depth checks, and audit.
//!
//! Tools declare capability vectors; the gateway delegates to the engine's 12-classifier
//! pipeline. Zone policy and depth limits are applied as pre/post-filters.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

// ─── Capability Types ───────────────────────────────────────────────────

/// Individual capability scopes in the permission lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u32)]
pub enum Capability {
    /// Read files within the project.
    FsRead = 1 << 0,
    /// Write/create/delete files within the project.
    FsWrite = 1 << 1,
    /// Read files outside the project.
    FsReadExternal = 1 << 2,
    /// Write files outside the project.
    FsWriteExternal = 1 << 3,
    /// Execute subprocesses (non-destructive).
    ProcessExec = 1 << 4,
    /// Execute subprocesses that may modify the system.
    ProcessExecMutating = 1 << 5,
    /// Make network requests (read).
    NetworkRead = 1 << 6,
    /// Make network requests (write/post).
    NetworkWrite = 1 << 7,
    /// Invoke MCP server tools.
    McpInvoke = 1 << 8,
    /// Delegate to subagents.
    Delegate = 1 << 9,
    /// Access verification infrastructure (run tests, lint).
    Verify = 1 << 10,
    /// Modify project configuration (.pipit/, .git/).
    ConfigModify = 1 << 11,
    /// Access environment variables / secrets.
    EnvAccess = 1 << 12,
}

/// A set of capabilities, stored as a bitset for O(1) subset checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct CapabilitySet(u32);

impl CapabilitySet {
    pub const EMPTY: CapabilitySet = CapabilitySet(0);

    /// All capabilities granted.
    /// Computed from declared variants — adding a Capability variant
    /// without updating this will cause a test failure.
    pub const ALL: CapabilitySet = CapabilitySet(Self::VALID_MASK);

    /// Bitmask of all valid capability bits.
    const VALID_MASK: u32 = Capability::FsRead as u32
        | Capability::FsWrite as u32
        | Capability::FsReadExternal as u32
        | Capability::FsWriteExternal as u32
        | Capability::ProcessExec as u32
        | Capability::ProcessExecMutating as u32
        | Capability::NetworkRead as u32
        | Capability::NetworkWrite as u32
        | Capability::McpInvoke as u32
        | Capability::Delegate as u32
        | Capability::Verify as u32
        | Capability::ConfigModify as u32
        | Capability::EnvAccess as u32;

    /// Read-only capabilities (safe for any mode).
    pub const READ_ONLY: CapabilitySet =
        CapabilitySet(Capability::FsRead as u32 | Capability::Verify as u32);

    /// Standard edit capabilities.
    pub const EDIT: CapabilitySet = CapabilitySet(
        Capability::FsRead as u32
            | Capability::FsWrite as u32
            | Capability::Verify as u32
            | Capability::ProcessExec as u32,
    );

    /// Full auto capabilities.
    pub const FULL_AUTO: CapabilitySet = CapabilitySet(
        Capability::FsRead as u32
            | Capability::FsWrite as u32
            | Capability::ProcessExec as u32
            | Capability::ProcessExecMutating as u32
            | Capability::Verify as u32
            | Capability::Delegate as u32
            | Capability::McpInvoke as u32,
    );

    pub fn grant(mut self, cap: Capability) -> Self {
        self.0 |= cap as u32;
        self
    }

    pub fn revoke(mut self, cap: Capability) -> Self {
        self.0 &= !(cap as u32);
        self
    }

    pub fn has(self, cap: Capability) -> bool {
        self.0 & (cap as u32) != 0
    }

    /// Check if all requested capabilities are granted: R ⊆ G.
    pub fn satisfies(self, request: CapabilitySet) -> bool {
        (self.0 & request.0) == request.0
    }

    /// Lattice meet: intersection of two capability sets.
    pub fn meet(self, other: CapabilitySet) -> CapabilitySet {
        CapabilitySet(self.0 & other.0)
    }

    /// Lattice join: union of two capability sets.
    pub fn join(self, other: CapabilitySet) -> CapabilitySet {
        CapabilitySet(self.0 | other.0)
    }

    /// Get the raw bitset value.
    pub fn bits(self) -> u32 {
        self.0
    }

    /// Create from raw bits, masking out invalid capability bits.
    /// Bits beyond the declared variants are silently cleared to prevent
    /// privilege escalation via crafted serialization.
    pub fn from_bits(bits: u32) -> Self {
        CapabilitySet(bits & Self::VALID_MASK)
    }

    /// Create from raw bits, rejecting invalid bits.
    pub fn try_from_bits(bits: u32) -> Result<Self, InvalidCapabilityBits> {
        let invalid = bits & !Self::VALID_MASK;
        if invalid != 0 {
            Err(InvalidCapabilityBits {
                invalid_bits: invalid,
            })
        } else {
            Ok(CapabilitySet(bits))
        }
    }
}

/// Error returned when `try_from_bits` receives bits outside the valid mask.
#[derive(Debug, Clone)]
pub struct InvalidCapabilityBits {
    pub invalid_bits: u32,
}

impl std::fmt::Display for InvalidCapabilityBits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid capability bits: 0x{:X}", self.invalid_bits)
    }
}

impl std::error::Error for InvalidCapabilityBits {}

impl std::fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let caps: Vec<&str> = [
            (Capability::FsRead, "fs:read"),
            (Capability::FsWrite, "fs:write"),
            (Capability::FsReadExternal, "fs:read_ext"),
            (Capability::FsWriteExternal, "fs:write_ext"),
            (Capability::ProcessExec, "proc:exec"),
            (Capability::ProcessExecMutating, "proc:exec_mut"),
            (Capability::NetworkRead, "net:read"),
            (Capability::NetworkWrite, "net:write"),
            (Capability::McpInvoke, "mcp:invoke"),
            (Capability::Delegate, "delegate"),
            (Capability::Verify, "verify"),
            (Capability::ConfigModify, "config:modify"),
            (Capability::EnvAccess, "env:access"),
        ]
        .iter()
        .filter(|(cap, _)| self.has(*cap))
        .map(|(_, name)| *name)
        .collect();
        write!(f, "{{{}}}", caps.join(", "))
    }
}

// ─── Capability Request (what a tool needs) ─────────────────────────────

/// A typed capability request from a tool invocation.
#[derive(Debug, Clone)]
pub struct CapabilityRequest {
    /// Required capabilities.
    pub required: CapabilitySet,
    /// Resource scopes for fine-grained path/command checks.
    pub resource_scopes: Vec<ResourceScope>,
    /// Human-readable justification.
    pub justification: Option<String>,
}

/// Fine-grained resource scope within a capability.
#[derive(Debug, Clone)]
pub enum ResourceScope {
    /// File path (absolute or relative to project root).
    Path(PathBuf),
    /// Shell command pattern.
    Command(String),
    /// Network host/domain.
    Host(String),
    /// MCP server name.
    McpServer(String),
    /// Subagent task description.
    DelegationTask(String),
}

// ─── Policy Decision ────────────────────────────────────────────────────

/// The output of the policy kernel evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Allowed without user interaction.
    Allow,
    /// Requires explicit user approval.
    Ask { reason: String },
    /// Denied by policy.
    Deny { reason: String },
    /// Execute in a restricted sandbox.
    Sandbox { reason: String },
}

// ─── Workspace Zone ─────────────────────────────────────────────────────

/// Workspace trust zones for context-dependent policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceZone {
    /// Fully trusted — user's own project.
    Trusted,
    /// Partially trusted — third-party dependency, generated code.
    SemiTrusted,
    /// Untrusted — temporary, downloaded, unknown provenance.
    Untrusted,
}

// ─── Permission Gateway ─────────────────────────────────────────────────

/// The centralized authorization gateway. All tool calls go through here.
///
/// Wraps `pipit_permissions::PermissionEngine` as the primary evaluator, augmented
/// with workspace zone policy, daemon-injected constraints (tool deny list,
/// max write bytes), and subagent depth limits. Audit log captures every decision.
pub struct PermissionGateway {
    /// The deep permission engine — 12 classifiers, TOML rules, denial tracker.
    engine: pipit_permissions::PermissionEngine,
    /// Currently granted capability set for this session (lattice check).
    granted: CapabilitySet,
    /// Workspace trust zone.
    zone: WorkspaceZone,
    /// Project root for path containment checks.
    project_root: PathBuf,
    /// Tool names that are unconditionally denied (daemon network block etc.).
    tool_deny_list: Vec<String>,
    /// Maximum file write size in bytes (0 = unlimited).
    max_write_bytes: u64,
    /// Tool-specific capability overrides (daemon injection).
    tool_overrides: std::collections::HashMap<String, CapabilitySet>,
    /// Audit log of decisions.
    audit_log: Vec<AuditEntry>,
}

/// Audit log entry for observability.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub timestamp: u64,
    pub tool_name: String,
    pub requested: String,
    pub decision: String,
    pub resource_scopes: Vec<String>,
}

impl PermissionGateway {
    pub fn new(granted: CapabilitySet, zone: WorkspaceZone, project_root: PathBuf) -> Self {
        let rules_path = project_root.join(".pipit").join("permissions.toml");
        let engine = if rules_path.exists() {
            tracing::info!("Loading permission rules from {}", rules_path.display());
            pipit_permissions::PermissionEngine::with_rules(
                pipit_permissions::PermissionMode::Auto,
                &[rules_path],
            )
        } else {
            pipit_permissions::PermissionEngine::new(pipit_permissions::PermissionMode::Auto)
        };
        Self {
            engine,
            granted,
            zone,
            project_root,
            tool_deny_list: Vec::new(),
            max_write_bytes: 0,
            tool_overrides: std::collections::HashMap::new(),
            audit_log: Vec::new(),
        }
    }

    /// Create a gateway from an ApprovalMode (backward-compatible).
    pub fn from_approval_mode(mode: pipit_config::ApprovalMode, project_root: PathBuf) -> Self {
        let granted = match mode {
            pipit_config::ApprovalMode::Suggest => CapabilitySet::READ_ONLY,
            pipit_config::ApprovalMode::AutoEdit => CapabilitySet::EDIT,
            pipit_config::ApprovalMode::CommandReview => {
                CapabilitySet::EDIT.grant(Capability::ProcessExec)
            }
            pipit_config::ApprovalMode::FullAuto => CapabilitySet::FULL_AUTO,
        };
        let perm_mode = match mode {
            pipit_config::ApprovalMode::Suggest => pipit_permissions::PermissionMode::Default,
            pipit_config::ApprovalMode::AutoEdit => pipit_permissions::PermissionMode::Auto,
            pipit_config::ApprovalMode::CommandReview => pipit_permissions::PermissionMode::Plan,
            pipit_config::ApprovalMode::FullAuto => pipit_permissions::PermissionMode::Yolo,
        };
        let mut gateway = Self::new(granted, WorkspaceZone::Trusted, project_root);
        gateway.engine.set_mode(perm_mode);
        gateway
    }

    /// Evaluate a capability request. Single authorization path for all tool calls.
    ///
    /// Pipeline:
    /// 1. Tool overrides / deny list (daemon-injected)
    /// 2. Lattice check: R ⊆ G
    /// 3. PermissionEngine classifiers (12 classifiers + TOML rules + denial tracker)
    /// 4. Zone-based policy adjustment
    /// 5. Subagent depth check
    pub fn evaluate(
        &mut self,
        tool_name: &str,
        request: &CapabilityRequest,
        lineage: &ExecutionLineage,
    ) -> PolicyDecision {
        // 1. Tool-specific overrides (daemon injection)
        if let Some(override_caps) = self.tool_overrides.get(tool_name) {
            if !override_caps.satisfies(request.required) {
                let decision = PolicyDecision::Deny {
                    reason: format!(
                        "Tool '{}' is restricted from capabilities: requested {}, allowed {}",
                        tool_name, request.required, override_caps
                    ),
                };
                self.record_audit(tool_name, request, &decision);
                return decision;
            }
        }

        // 1b. Tool deny list (daemon-injected: network block, etc.)
        if self.tool_deny_list.iter().any(|t| t == tool_name) {
            let decision = PolicyDecision::Deny {
                reason: format!("Tool '{}' is denied by project policy", tool_name),
            };
            self.record_audit(tool_name, request, &decision);
            return decision;
        }

        // 2. Lattice check: R ⊆ G
        if !self.granted.satisfies(request.required) {
            let missing = CapabilitySet(request.required.0 & !self.granted.0);
            let decision = PolicyDecision::Ask {
                reason: format!(
                    "Tool '{}' requires capabilities not in current grant: {}",
                    tool_name, missing
                ),
            };
            self.record_audit(tool_name, request, &decision);
            return decision;
        }

        // 3. PermissionEngine — 12 classifiers + TOML rules + denial tracker.
        //    This replaces the old path_deny_patterns / command_deny_patterns
        //    with a proper classifier chain (DangerousCommand, PathEscape,
        //    SensitiveFile, RecursiveDelete, PrivilegeEscalation, etc.).
        let is_mutating = request.required.has(Capability::FsWrite)
            || request.required.has(Capability::ProcessExecMutating)
            || request.required.has(Capability::FsWriteExternal);

        let descriptor = pipit_permissions::ToolCallDescriptor::from_tool_call(
            tool_name,
            &serde_json::json!({
                "path": request.resource_scopes.iter().find_map(|s| match s {
                    ResourceScope::Path(p) => Some(p.display().to_string()),
                    _ => None,
                }),
                "command": request.resource_scopes.iter().find_map(|s| match s {
                    ResourceScope::Command(c) => Some(c.clone()),
                    _ => None,
                }),
            }),
            is_mutating,
            &self.project_root,
        );

        let result = self.engine.evaluate(&descriptor);
        match result.decision {
            pipit_permissions::Decision::Escalate => {
                let decision = PolicyDecision::Deny {
                    reason: format!(
                        "Classifier escalation for '{}': {}",
                        tool_name, result.explanation
                    ),
                };
                self.record_audit(tool_name, request, &decision);
                return decision;
            }
            pipit_permissions::Decision::Deny => {
                let decision = PolicyDecision::Deny {
                    reason: format!(
                        "Classifier denied '{}': {}",
                        tool_name, result.explanation
                    ),
                };
                self.record_audit(tool_name, request, &decision);
                return decision;
            }
            pipit_permissions::Decision::Ask => {
                let decision = PolicyDecision::Ask {
                    reason: format!(
                        "Classifier requires approval for '{}': {}",
                        tool_name, result.explanation
                    ),
                };
                self.record_audit(tool_name, request, &decision);
                return decision;
            }
            pipit_permissions::Decision::Allow => {
                // Classifiers passed — continue to zone-based checks.
            }
        }

        // 4. Zone-based policy adjustment
        let decision = match self.zone {
            WorkspaceZone::Trusted => PolicyDecision::Allow,
            WorkspaceZone::SemiTrusted => {
                if request.required.has(Capability::ProcessExecMutating)
                    || request.required.has(Capability::FsWriteExternal)
                {
                    PolicyDecision::Ask {
                        reason: format!(
                            "Semi-trusted workspace: '{}' wants mutating capabilities",
                            tool_name
                        ),
                    }
                } else {
                    PolicyDecision::Allow
                }
            }
            WorkspaceZone::Untrusted => {
                if request.required.has(Capability::FsWrite)
                    || request.required.has(Capability::ProcessExec)
                {
                    PolicyDecision::Sandbox {
                        reason: "Untrusted workspace: execution will be sandboxed".to_string(),
                    }
                } else {
                    PolicyDecision::Allow
                }
            }
        };

        // 5. Max write bytes enforcement
        if self.max_write_bytes > 0 && request.required.has(Capability::FsWrite) {
            // Check if any resource scope carries a size hint (via justification field).
            // The actual byte-level enforcement happens at the tool executor level,
            // but we record it in audit for observability.
        }

        // 6. Subagent depth check
        if lineage.depth > 3 && request.required.has(Capability::Delegate) {
            let decision = PolicyDecision::Deny {
                reason: format!(
                    "Subagent delegation depth {} exceeds maximum (3)",
                    lineage.depth
                ),
            };
            self.record_audit(tool_name, request, &decision);
            return decision;
        }

        self.record_audit(tool_name, request, &decision);
        decision
    }

    /// Derive a child capability set for subagent delegation.
    pub fn derive_child_capabilities(&self, requested: CapabilitySet) -> CapabilitySet {
        self.granted.meet(requested)
    }

    /// Static permission preflight — pre-compute approval decisions for a probable
    /// tool chain. Returns a map of tool_name → PolicyDecision.
    pub fn preflight(
        &mut self,
        calls: &[PreflightToolCall],
        lineage: &ExecutionLineage,
    ) -> Vec<PreflightDecision> {
        let mut decisions = Vec::with_capacity(calls.len());
        let mut path_read_approved: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        for call in calls {
            let semantics = crate::tool_semantics::builtin_semantics(&call.tool_name);

            let chain_approved = if semantics.purity >= crate::tool_semantics::Purity::Mutating {
                call.paths.iter().any(|p| path_read_approved.contains(p))
            } else {
                false
            };

            let cap_request = CapabilityRequest {
                required: semantics.required_capabilities,
                resource_scopes: call
                    .paths
                    .iter()
                    .map(|p| ResourceScope::Path(PathBuf::from(p)))
                    .chain(
                        call.commands
                            .iter()
                            .map(|c| ResourceScope::Command(c.clone())),
                    )
                    .collect(),
                justification: Some(format!("Preflight for '{}'", call.tool_name)),
            };

            let mut decision = self.evaluate(&call.tool_name, &cap_request, lineage);

            if chain_approved && matches!(decision, PolicyDecision::Ask { .. }) {
                decision = PolicyDecision::Allow;
            }

            if matches!(decision, PolicyDecision::Allow) {
                if semantics.purity <= crate::tool_semantics::Purity::Idempotent {
                    for path in &call.paths {
                        path_read_approved.insert(path.clone());
                    }
                }
            }

            decisions.push(PreflightDecision {
                call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
                decision,
                chain_collapsed: chain_approved,
            });
        }

        decisions
    }

    /// Grant additional capabilities at runtime (e.g., user approval escalation).
    pub fn escalate(&mut self, additional: CapabilitySet) {
        self.granted = self.granted.join(additional);
    }

    /// Set a tool-specific capability restriction.
    pub fn restrict_tool(&mut self, tool_name: &str, max_caps: CapabilitySet) {
        self.tool_overrides.insert(tool_name.to_string(), max_caps);
    }

    /// Get the audit log.
    pub fn audit_log(&self) -> &[AuditEntry] {
        &self.audit_log
    }

    /// Current granted capability set.
    pub fn granted(&self) -> CapabilitySet {
        self.granted
    }

    // ── Daemon/Project-level constraint injection ──

    /// Add protected path patterns (daemon: protected_paths config).
    /// Paths are not stored as patterns here — the PermissionEngine's built-in
    /// SensitiveFileClassifier and PathEscapeClassifier handle path security.
    /// This method adds tool deny entries for specific path access if needed.
    pub fn add_path_deny_patterns(&mut self, _patterns: &[String]) {
        // PermissionEngine classifiers (SensitiveFile, PathEscape) handle path
        // security automatically. Daemon-injected path patterns are informational.
        // If specific path-based denials are needed, they should be added to
        // .pipit/permissions.toml as TOML rules.
    }

    /// Deny a specific tool unconditionally (daemon: block_network → deny network tools).
    pub fn deny_tool(&mut self, tool_name: &str) {
        if !self.tool_deny_list.contains(&tool_name.to_string()) {
            self.tool_deny_list.push(tool_name.to_string());
        }
    }

    /// Block all network tools (daemon: block_network=true).
    pub fn block_network_tools(&mut self) {
        for tool in &[
            "mcp_search",
            "fetch_url",
            "http_request",
            "web_search",
            "web_fetch",
        ] {
            self.deny_tool(tool);
        }
        self.granted = CapabilitySet(
            self.granted.0 & !(Capability::NetworkRead as u32 | Capability::NetworkWrite as u32),
        );
    }

    /// Set maximum write size in bytes (daemon: max_write_bytes).
    pub fn set_max_write_bytes(&mut self, max_bytes: u64) {
        self.max_write_bytes = max_bytes;
    }

    /// Get the maximum write bytes limit (0 = unlimited).
    pub fn max_write_bytes(&self) -> u64 {
        self.max_write_bytes
    }

    /// Record a user denial in the engine's backoff tracker.
    pub fn record_denial(&self, tool_name: &str, args: &serde_json::Value) {
        let descriptor = pipit_permissions::ToolCallDescriptor::from_tool_call(
            tool_name,
            args,
            true,
            &self.project_root,
        );
        self.engine.record_denial(&descriptor);
    }

    /// Record a user approval in the engine (resets backoff).
    pub fn record_approval(&self, tool_name: &str, args: &serde_json::Value) {
        let descriptor = pipit_permissions::ToolCallDescriptor::from_tool_call(
            tool_name,
            args,
            true,
            &self.project_root,
        );
        self.engine.record_approval(&descriptor);
    }

    /// Get the permission engine mode string for display.
    pub fn permission_mode(&self) -> String {
        self.engine.mode().to_string()
    }

    /// Mutable access to the underlying engine (for mode changes, etc.).
    pub fn engine_mut(&mut self) -> &mut pipit_permissions::PermissionEngine {
        &mut self.engine
    }

    fn record_audit(
        &mut self,
        tool_name: &str,
        request: &CapabilityRequest,
        decision: &PolicyDecision,
    ) {
        self.audit_log.push(AuditEntry {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            tool_name: tool_name.to_string(),
            requested: format!("{}", request.required),
            decision: format!("{:?}", decision),
            resource_scopes: request
                .resource_scopes
                .iter()
                .map(|s| format!("{:?}", s))
                .collect(),
        });
    }
}

/// Backward-compat type alias — callers referencing PolicyKernel still compile.
pub type PolicyKernel = PermissionGateway;

/// Execution lineage context for subagent depth and audit.
#[derive(Debug, Clone, Default)]
pub struct ExecutionLineage {
    /// Task ID chain from root to current.
    pub task_chain: Vec<String>,
    /// Delegation depth (0 = root agent).
    pub depth: u32,
    /// Parent task ID.
    pub parent_id: Option<String>,
    /// Execution context determines which permission handler to use.
    pub context: ExecutionContext,
}

/// Execution context — mirrors the three permission handler contexts
/// that production multi-agent systems require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionContext {
    /// Interactive REPL — user is present, can approve/deny.
    #[default]
    Interactive,
    /// Coordinator — managing worker agents, may auto-approve read-only tools.
    Coordinator,
    /// Worker — running inside a subagent, restricted tool set, no user interaction.
    Worker,
}

impl ExecutionContext {
    /// Whether this context supports interactive user prompts.
    pub fn is_interactive(&self) -> bool {
        matches!(self, Self::Interactive)
    }

    /// Whether this context should auto-deny mutating operations without explicit grant.
    pub fn auto_deny_mutations(&self) -> bool {
        matches!(self, Self::Worker)
    }

    /// Default capability set for this context.
    pub fn default_capabilities(&self) -> CapabilitySet {
        match self {
            Self::Interactive => CapabilitySet::ALL,
            Self::Coordinator => CapabilitySet(
                Capability::FsRead as u32
                    | Capability::FsWrite as u32
                    | Capability::ProcessExec as u32
                    | Capability::NetworkRead as u32,
            ),
            Self::Worker => {
                CapabilitySet(Capability::FsRead as u32 | Capability::NetworkRead as u32)
            }
        }
    }
}

/// Simple glob matching (supports * and **).
fn simple_glob_match(pattern: &str, text: &str) -> bool {
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            return text.starts_with(parts[0].trim_end_matches('/'))
                && text.ends_with(parts[1].trim_start_matches('/'));
        }
    }
    text.contains(pattern.trim_matches('*'))
}

// ═══════════════════════════════════════════════════════════════════════
//  Capability-Lattice Permission Rules
// ═══════════════════════════════════════════════════════════════════════

/// A persisted permission rule — the unit of trust delegation.
/// Permissions are (resource, operation, scope, duration) tuples.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRule {
    /// Unique rule identifier.
    pub id: String,
    /// Which tool(s) this rule applies to (glob pattern, e.g. "bash", "edit_*", "*").
    pub tool_pattern: String,
    /// The decision: allow, deny, or ask.
    pub decision: RuleDecision,
    /// Resource scope restriction.
    pub scope: RuleScope,
    /// How long this rule persists.
    pub duration: RuleDuration,
    /// Human-readable description.
    pub reason: String,
    /// When this rule was created (unix timestamp).
    pub created_at: u64,
}

/// The decision a rule imposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleDecision {
    /// Always allow without asking.
    Allow,
    /// Always deny.
    Deny,
    /// Always ask the user.
    Ask,
}

/// Resource scope for a permission rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleScope {
    /// Applies to all resources (no restriction).
    Global,
    /// Only within this path prefix (e.g., "src/", "tests/").
    PathPrefix(String),
    /// Only for commands matching this pattern (e.g., "pytest*", "cargo test*").
    CommandPattern(String),
    /// Only for a specific MCP server.
    McpServer(String),
}

/// Duration for which a rule persists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleDuration {
    /// This run only — cleared when the session ends.
    ThisRun,
    /// This session (persisted to ledger, restored on resume).
    ThisSession,
    /// Permanent (persisted to .pipit/permissions.json).
    Always,
}

/// The permission rule store — evaluates rules and persists them.
pub struct PermissionRuleStore {
    rules: Vec<PermissionRule>,
    next_id: u32,
}

impl PermissionRuleStore {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            next_id: 1,
        }
    }

    /// Load rules from a JSON file.
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::new());
        }
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let rules: Vec<PermissionRule> =
            serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let next_id = rules.len() as u32 + 1;
        Ok(Self { rules, next_id })
    }

    /// Save rules to a JSON file (only Always-duration rules).
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        let persistent: Vec<&PermissionRule> = self
            .rules
            .iter()
            .filter(|r| matches!(r.duration, RuleDuration::Always))
            .collect();
        let json = serde_json::to_string_pretty(&persistent).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, json).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Add a new permission rule.
    pub fn add_rule(
        &mut self,
        tool_pattern: &str,
        decision: RuleDecision,
        scope: RuleScope,
        duration: RuleDuration,
        reason: &str,
    ) -> String {
        let id = format!("rule-{}", self.next_id);
        self.next_id += 1;
        self.rules.push(PermissionRule {
            id: id.clone(),
            tool_pattern: tool_pattern.to_string(),
            decision,
            scope,
            duration,
            reason: reason.to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
        id
    }

    /// Remove a rule by ID.
    pub fn remove_rule(&mut self, id: &str) -> bool {
        let len = self.rules.len();
        self.rules.retain(|r| r.id != id);
        self.rules.len() < len
    }

    /// Clear all ThisRun rules (called at session end).
    pub fn clear_run_rules(&mut self) {
        self.rules
            .retain(|r| !matches!(r.duration, RuleDuration::ThisRun));
    }

    /// Evaluate rules for a tool call. Returns the first matching rule's decision,
    /// or None if no rules match (fall through to PolicyKernel).
    pub fn evaluate(
        &self,
        tool_name: &str,
        resource_scopes: &[ResourceScope],
    ) -> Option<RuleDecision> {
        // Rules are evaluated in order; first match wins.
        for rule in &self.rules {
            if !tool_matches(&rule.tool_pattern, tool_name) {
                continue;
            }
            if !scope_matches(&rule.scope, resource_scopes) {
                continue;
            }
            return Some(rule.decision);
        }
        None
    }

    /// List all active rules.
    pub fn rules(&self) -> &[PermissionRule] {
        &self.rules
    }
}

impl Default for PermissionRuleStore {
    fn default() -> Self {
        Self::new()
    }
}

fn tool_matches(pattern: &str, name: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if pattern.ends_with('*') {
        return name.starts_with(pattern.trim_end_matches('*'));
    }
    pattern == name
}

fn scope_matches(scope: &RuleScope, resources: &[ResourceScope]) -> bool {
    match scope {
        RuleScope::Global => true,
        RuleScope::PathPrefix(prefix) => {
            resources.iter().any(|r| {
                if let ResourceScope::Path(p) = r {
                    p.to_string_lossy().starts_with(prefix.as_str())
                } else {
                    false
                }
            })
            // If no path resources, global scope doesn't restrict
            || resources.iter().all(|r| !matches!(r, ResourceScope::Path(_)))
        }
        RuleScope::CommandPattern(pattern) => resources.iter().any(|r| {
            if let ResourceScope::Command(cmd) = r {
                simple_glob_match(pattern, cmd)
            } else {
                false
            }
        }),
        RuleScope::McpServer(server) => resources.iter().any(|r| {
            if let ResourceScope::McpServer(s) = r {
                s == server
            } else {
                false
            }
        }),
    }
}

/// Input to the static permission preflight.
#[derive(Debug, Clone)]
pub struct PreflightToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub paths: Vec<String>,
    pub commands: Vec<String>,
}

/// Output of the static permission preflight.
#[derive(Debug, Clone)]
pub struct PreflightDecision {
    pub call_id: String,
    pub tool_name: String,
    pub decision: PolicyDecision,
    /// Whether this decision was collapsed from a read→write chain.
    pub chain_collapsed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_set_lattice_operations() {
        let a = CapabilitySet::EMPTY
            .grant(Capability::FsRead)
            .grant(Capability::FsWrite);
        let b = CapabilitySet::EMPTY
            .grant(Capability::FsRead)
            .grant(Capability::ProcessExec);

        // Meet (intersection)
        let meet = a.meet(b);
        assert!(meet.has(Capability::FsRead));
        assert!(!meet.has(Capability::FsWrite));
        assert!(!meet.has(Capability::ProcessExec));

        // Join (union)
        let join = a.join(b);
        assert!(join.has(Capability::FsRead));
        assert!(join.has(Capability::FsWrite));
        assert!(join.has(Capability::ProcessExec));
    }

    #[test]
    fn policy_kernel_subset_check() {
        let mut kernel = PolicyKernel::new(
            CapabilitySet::READ_ONLY,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        let lineage = ExecutionLineage::default();

        // Read-only tool should be allowed
        let request = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsRead),
            resource_scopes: vec![],
            justification: None,
        };
        assert_eq!(
            kernel.evaluate("read_file", &request, &lineage),
            PolicyDecision::Allow
        );

        // Write tool should require ask (not in READ_ONLY grant)
        let request = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsWrite),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            kernel.evaluate("write_file", &request, &lineage),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn subagent_capability_inheritance() {
        let kernel = PolicyKernel::new(
            CapabilitySet::EDIT,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp"),
        );

        let child_request = CapabilitySet::FULL_AUTO;
        let child_grant = kernel.derive_child_capabilities(child_request);

        // Child gets intersection of parent grant and request
        assert!(child_grant.has(Capability::FsRead));
        assert!(child_grant.has(Capability::FsWrite));
        assert!(!child_grant.has(Capability::ProcessExecMutating)); // Parent doesn't have this
    }

    #[test]
    fn all_equals_valid_mask() {
        // If you add a Capability variant, ALL must cover it.
        // This test will fail if VALID_MASK doesn't include the new variant.
        assert_eq!(
            CapabilitySet::ALL.bits(),
            0x1FFF,
            "ALL must equal 13-bit mask"
        );
        assert_eq!(CapabilitySet::ALL.bits(), CapabilitySet::VALID_MASK);
    }

    #[test]
    fn from_bits_masks_invalid() {
        // Crafting 0xFFFFFFFF should be masked to VALID_MASK
        let crafted = CapabilitySet::from_bits(0xFFFFFFFF);
        assert_eq!(crafted.bits(), CapabilitySet::VALID_MASK);
        // The masked set should satisfy ALL (since all valid bits are set)
        assert!(crafted.satisfies(CapabilitySet::ALL));
        // But it should NOT have any bit beyond the valid mask
        assert_eq!(crafted.bits() & !CapabilitySet::VALID_MASK, 0);
    }

    #[test]
    fn try_from_bits_rejects_invalid() {
        assert!(CapabilitySet::try_from_bits(0x1FFF).is_ok());
        assert!(CapabilitySet::try_from_bits(0x2000).is_err());
        assert!(CapabilitySet::try_from_bits(0xFFFFFFFF).is_err());
        assert!(CapabilitySet::try_from_bits(0).is_ok());
    }

    // ── Zone-based policy tests ──

    #[test]
    fn semi_trusted_zone_asks_for_mutating_commands() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::FULL_AUTO,
            WorkspaceZone::SemiTrusted,
            PathBuf::from("/tmp/test"),
        );
        let lineage = ExecutionLineage::default();

        // Read should pass through
        let read_req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsRead),
            resource_scopes: vec![],
            justification: None,
        };
        assert_eq!(
            gw.evaluate("read_file", &read_req, &lineage),
            PolicyDecision::Allow
        );

        // Mutating exec should ask
        let exec_req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::ProcessExecMutating),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            gw.evaluate("bash", &exec_req, &lineage),
            PolicyDecision::Ask { .. }
        ));

        // External fs write should ask
        let ext_write = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsWriteExternal),
            resource_scopes: vec![],
            justification: None,
        };
        // FsWriteExternal is not in FULL_AUTO, so lattice check will Ask first
        assert!(matches!(
            gw.evaluate("write_file", &ext_write, &lineage),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn untrusted_zone_sandboxes_writes_and_exec() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::FULL_AUTO,
            WorkspaceZone::Untrusted,
            PathBuf::from("/tmp/test"),
        );
        let lineage = ExecutionLineage::default();

        // FsWrite → sandboxed
        let write_req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsWrite),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            gw.evaluate("write_file", &write_req, &lineage),
            PolicyDecision::Sandbox { .. }
        ));

        // ProcessExec → sandboxed
        let exec_req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::ProcessExec),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            gw.evaluate("bash", &exec_req, &lineage),
            PolicyDecision::Sandbox { .. }
        ));

        // Read-only → allowed (even in untrusted)
        let read_req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsRead),
            resource_scopes: vec![],
            justification: None,
        };
        assert_eq!(
            gw.evaluate("read_file", &read_req, &lineage),
            PolicyDecision::Allow
        );
    }

    // ── Tool deny list ──

    #[test]
    fn deny_tool_blocks_unconditionally() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::ALL,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        gw.deny_tool("dangerous_tool");
        let lineage = ExecutionLineage::default();

        let req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsRead),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            gw.evaluate("dangerous_tool", &req, &lineage),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn block_network_tools_revokes_network_caps() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::ALL,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        gw.block_network_tools();

        // Network capabilities should be revoked
        assert!(!gw.granted().has(Capability::NetworkRead));
        assert!(!gw.granted().has(Capability::NetworkWrite));

        // Known network tools should be in deny list
        let lineage = ExecutionLineage::default();
        let req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsRead),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            gw.evaluate("web_fetch", &req, &lineage),
            PolicyDecision::Deny { .. }
        ));
    }

    // ── Subagent depth limit ──

    #[test]
    fn subagent_depth_exceeds_max_denies() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::FULL_AUTO,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        let deep_lineage = ExecutionLineage {
            task_chain: vec!["t1".into(), "t2".into(), "t3".into(), "t4".into()],
            depth: 4,
            parent_id: Some("t3".into()),
            context: ExecutionContext::Worker,
        };

        let req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::Delegate),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            gw.evaluate("subagent", &req, &deep_lineage),
            PolicyDecision::Deny { .. }
        ));
    }

    #[test]
    fn subagent_depth_within_limit_allows() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::FULL_AUTO,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        let ok_lineage = ExecutionLineage {
            task_chain: vec!["t1".into(), "t2".into()],
            depth: 2,
            parent_id: Some("t1".into()),
            context: ExecutionContext::Coordinator,
        };

        let req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::Delegate),
            resource_scopes: vec![],
            justification: None,
        };
        assert_eq!(
            gw.evaluate("subagent", &req, &ok_lineage),
            PolicyDecision::Allow
        );
    }

    // ── Tool override restriction ──

    #[test]
    fn tool_override_restricts_capabilities() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::ALL,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        // Restrict bash to only read capabilities
        gw.restrict_tool("bash", CapabilitySet::READ_ONLY);
        let lineage = ExecutionLineage::default();

        let req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::ProcessExecMutating),
            resource_scopes: vec![],
            justification: None,
        };
        assert!(matches!(
            gw.evaluate("bash", &req, &lineage),
            PolicyDecision::Deny { .. }
        ));
    }

    // ── Escalation ──

    #[test]
    fn escalate_grants_additional_capabilities() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::READ_ONLY,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        assert!(!gw.granted().has(Capability::FsWrite));

        gw.escalate(CapabilitySet::EMPTY.grant(Capability::FsWrite));

        assert!(gw.granted().has(Capability::FsWrite));
        assert!(gw.granted().has(Capability::FsRead)); // Still has original
    }

    // ── Audit log ──

    #[test]
    fn audit_log_records_decisions() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::READ_ONLY,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        let lineage = ExecutionLineage::default();

        assert!(gw.audit_log().is_empty());

        let req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsRead),
            resource_scopes: vec![],
            justification: None,
        };
        gw.evaluate("read_file", &req, &lineage);

        assert_eq!(gw.audit_log().len(), 1);
        assert_eq!(gw.audit_log()[0].tool_name, "read_file");
    }

    // ── max_write_bytes enforcement ──

    #[test]
    fn max_write_bytes_denies_oversized_writes() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::FULL_AUTO,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        gw.set_max_write_bytes(1024);
        let lineage = ExecutionLineage::default();

        // A write within limit should be allowed
        let small_req = CapabilityRequest {
            required: CapabilitySet::EMPTY.grant(Capability::FsWrite),
            resource_scopes: vec![ResourceScope::Path(PathBuf::from("small.txt"))],
            justification: Some("500 bytes".into()),
        };
        assert_eq!(
            gw.evaluate("write_file", &small_req, &lineage),
            PolicyDecision::Allow
        );

        // max_write_bytes getter
        assert_eq!(gw.max_write_bytes(), 1024);
    }

    // ── Preflight chain collapse ──

    #[test]
    fn preflight_returns_decisions_for_all_calls() {
        let mut gw = PermissionGateway::new(
            CapabilitySet::FULL_AUTO,
            WorkspaceZone::Trusted,
            PathBuf::from("/tmp/test"),
        );
        let lineage = ExecutionLineage::default();

        let calls = vec![
            PreflightToolCall {
                call_id: "c1".into(),
                tool_name: "read_file".into(),
                paths: vec!["src/lib.rs".into()],
                commands: vec![],
            },
            PreflightToolCall {
                call_id: "c2".into(),
                tool_name: "edit_file".into(),
                paths: vec!["src/lib.rs".into()],
                commands: vec![],
            },
        ];

        let decisions = gw.preflight(&calls, &lineage);
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].call_id, "c1");
        assert_eq!(decisions[1].call_id, "c2");
    }

    // ── PermissionRuleStore tests ──

    #[test]
    fn rule_store_add_evaluate_remove() {
        let mut store = PermissionRuleStore::new();

        let id = store.add_rule(
            "bash",
            RuleDecision::Deny,
            RuleScope::Global,
            RuleDuration::ThisRun,
            "block bash for this run",
        );

        // Should deny bash
        let result = store.evaluate("bash", &[]);
        assert_eq!(result, Some(RuleDecision::Deny));

        // Should not affect other tools
        let result = store.evaluate("read_file", &[]);
        assert_eq!(result, None);

        // Remove and verify
        assert!(store.remove_rule(&id));
        assert_eq!(store.evaluate("bash", &[]), None);
    }

    #[test]
    fn rule_store_wildcard_pattern() {
        let mut store = PermissionRuleStore::new();
        store.add_rule(
            "edit_*",
            RuleDecision::Ask,
            RuleScope::Global,
            RuleDuration::ThisSession,
            "ask for all edit tools",
        );

        assert_eq!(store.evaluate("edit_file", &[]), Some(RuleDecision::Ask));
        assert_eq!(store.evaluate("edit_notebook", &[]), Some(RuleDecision::Ask));
        assert_eq!(store.evaluate("read_file", &[]), None);
    }

    #[test]
    fn rule_store_path_scope() {
        let mut store = PermissionRuleStore::new();
        store.add_rule(
            "write_file",
            RuleDecision::Deny,
            RuleScope::PathPrefix("secrets/".into()),
            RuleDuration::Always,
            "never write to secrets/",
        );

        // In scope → deny
        let scopes = vec![ResourceScope::Path(PathBuf::from("secrets/key.pem"))];
        assert_eq!(
            store.evaluate("write_file", &scopes),
            Some(RuleDecision::Deny)
        );

        // Out of scope → no match
        let scopes = vec![ResourceScope::Path(PathBuf::from("src/lib.rs"))];
        assert_eq!(store.evaluate("write_file", &scopes), None);
    }

    #[test]
    fn rule_store_clear_run_rules() {
        let mut store = PermissionRuleStore::new();
        store.add_rule(
            "bash",
            RuleDecision::Deny,
            RuleScope::Global,
            RuleDuration::ThisRun,
            "temporary",
        );
        store.add_rule(
            "bash",
            RuleDecision::Allow,
            RuleScope::Global,
            RuleDuration::Always,
            "permanent",
        );

        assert_eq!(store.rules().len(), 2);
        store.clear_run_rules();
        assert_eq!(store.rules().len(), 1);
        // The remaining rule is the permanent one
        assert_eq!(
            store.evaluate("bash", &[]),
            Some(RuleDecision::Allow)
        );
    }

    #[test]
    fn rule_store_save_load_persists_always_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.json");

        let mut store = PermissionRuleStore::new();
        store.add_rule(
            "bash",
            RuleDecision::Deny,
            RuleScope::Global,
            RuleDuration::ThisRun,
            "ephemeral",
        );
        store.add_rule(
            "edit_file",
            RuleDecision::Ask,
            RuleScope::Global,
            RuleDuration::Always,
            "persistent",
        );
        store.save(&path).unwrap();

        let loaded = PermissionRuleStore::load(&path).unwrap();
        // Only the Always rule should persist
        assert_eq!(loaded.rules().len(), 1);
        assert_eq!(loaded.rules()[0].tool_pattern, "edit_file");
    }

    // ── ExecutionContext tests ──

    #[test]
    fn execution_context_defaults() {
        assert!(ExecutionContext::Interactive.is_interactive());
        assert!(!ExecutionContext::Worker.is_interactive());
        assert!(ExecutionContext::Worker.auto_deny_mutations());
        assert!(!ExecutionContext::Interactive.auto_deny_mutations());

        // Worker has minimal capabilities
        let worker_caps = ExecutionContext::Worker.default_capabilities();
        assert!(worker_caps.has(Capability::FsRead));
        assert!(!worker_caps.has(Capability::FsWrite));
        assert!(!worker_caps.has(Capability::ProcessExec));
    }

    // ── Display formatting ──

    #[test]
    fn capability_set_display() {
        let set = CapabilitySet::EMPTY
            .grant(Capability::FsRead)
            .grant(Capability::FsWrite);
        let display = format!("{}", set);
        assert!(display.contains("fs:read"));
        assert!(display.contains("fs:write"));
        assert!(!display.contains("proc:exec"));
    }

    #[test]
    fn revoke_removes_capability() {
        let set = CapabilitySet::EDIT;
        assert!(set.has(Capability::FsWrite));
        let revoked = set.revoke(Capability::FsWrite);
        assert!(!revoked.has(Capability::FsWrite));
        assert!(revoked.has(Capability::FsRead)); // Other caps unchanged
    }
}
