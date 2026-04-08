//! Capability-Lattice Permission Kernel (Architecture Task 1)
//!
//! Replaces per-tool boolean approval checks with a centralized policy kernel.
//! Every action is evaluated against a typed capability request: filesystem
//! read/write scope, process execution scope, network scope, etc.
//!
//! Tools declare capability vectors; the kernel evaluates `R ⊆ G` where R is
//! the requested capability set and G is the granted set. For practical widths,
//! this is O(1) via bitset meet.

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
    pub const ALL: CapabilitySet = CapabilitySet(0x1FFF); // 13 bits

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

    /// Create from raw bits.
    pub fn from_bits(bits: u32) -> Self {
        CapabilitySet(bits)
    }
}

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

// ─── Policy Kernel ──────────────────────────────────────────────────────

/// The centralized permission kernel. All authorization goes through here.
pub struct PolicyKernel {
    /// Currently granted capability set for this session.
    granted: CapabilitySet,
    /// Workspace trust zone.
    zone: WorkspaceZone,
    /// Project root for path containment checks.
    project_root: PathBuf,
    /// Path deny-list patterns (glob-style).
    path_deny_patterns: Vec<String>,
    /// Command deny-list patterns.
    command_deny_patterns: Vec<String>,
    /// Tool-specific capability overrides.
    tool_overrides: std::collections::HashMap<String, CapabilitySet>,
    /// Tool names that are unconditionally denied (daemon network block etc.).
    tool_deny_list: Vec<String>,
    /// Maximum file write size in bytes (0 = unlimited).
    max_write_bytes: u64,
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

impl PolicyKernel {
    pub fn new(granted: CapabilitySet, zone: WorkspaceZone, project_root: PathBuf) -> Self {
        Self {
            granted,
            zone,
            project_root,
            path_deny_patterns: vec![
                "**/.git/objects/**".to_string(),
                "**/.pipit/credentials*".to_string(),
                "**/node_modules/.cache/**".to_string(),
            ],
            command_deny_patterns: vec![
                "rm -rf /".to_string(),
                "rm -rf /*".to_string(),
                "mkfs*".to_string(),
                "dd if=*of=/dev/*".to_string(),
                ":(){:|:&};:".to_string(),
            ],
            tool_overrides: std::collections::HashMap::new(),
            tool_deny_list: Vec::new(),
            max_write_bytes: 0,
            audit_log: Vec::new(),
        }
    }

    /// Create a kernel from an ApprovalMode (backward-compatible).
    pub fn from_approval_mode(mode: pipit_config::ApprovalMode, project_root: PathBuf) -> Self {
        let granted = match mode {
            pipit_config::ApprovalMode::Suggest => CapabilitySet::READ_ONLY,
            pipit_config::ApprovalMode::AutoEdit => CapabilitySet::EDIT,
            pipit_config::ApprovalMode::CommandReview => {
                CapabilitySet::EDIT.grant(Capability::ProcessExec)
            }
            pipit_config::ApprovalMode::FullAuto => CapabilitySet::FULL_AUTO,
        };
        Self::new(granted, WorkspaceZone::Trusted, project_root)
    }

    /// Evaluate a capability request against the current policy.
    pub fn evaluate(
        &mut self,
        tool_name: &str,
        request: &CapabilityRequest,
        lineage: &ExecutionLineage,
    ) -> PolicyDecision {
        // 1. Check tool-specific overrides
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

        // 1b. Check tool deny list (daemon-injected: network block, etc.)
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

        // 3. Fine-grained resource scope checks
        for scope in &request.resource_scopes {
            match scope {
                ResourceScope::Path(path) => {
                    if let Some(decision) = self.check_path(tool_name, path) {
                        self.record_audit(tool_name, request, &decision);
                        return decision;
                    }
                }
                ResourceScope::Command(cmd) => {
                    if let Some(decision) = self.check_command(tool_name, cmd) {
                        self.record_audit(tool_name, request, &decision);
                        return decision;
                    }
                }
                _ => {}
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

        // 5. Subagent depth check
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
    /// Child inherits the meet (intersection) of parent's grant and requested capabilities.
    pub fn derive_child_capabilities(&self, requested: CapabilitySet) -> CapabilitySet {
        self.granted.meet(requested)
    }

    /// Static permission preflight — pre-compute approval decisions for a probable
    /// tool chain. Returns a map of tool_name → PolicyDecision.
    ///
    /// Call this BEFORE tool execution starts. Tools that get `Allow` in preflight
    /// can execute without mid-turn approval stalls. Tools that get `Ask` still
    /// need runtime confirmation, but the user can be prompted once for the whole
    /// batch rather than one-by-one.
    ///
    /// Safe chains like `read_file → edit_file` on the same path are collapsed
    /// into a single approval decision for the chain.
    ///
    /// Complexity: O(n) in the number of tool calls, O(1) per lattice check.
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

            // Check if this is a read→write chain on an already-approved path
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

            // Upgrade Ask→Allow for safe chains (read on same path was already preflighted)
            if chain_approved && matches!(decision, PolicyDecision::Ask { .. }) {
                decision = PolicyDecision::Allow;
            }

            // Track read approvals for chain collapsing
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
    // These methods let the daemon push its project-level policy
    // into the single PolicyKernel, eliminating parallel authorize logic.

    /// Add a path pattern to the deny list (daemon: protected_paths).
    pub fn add_path_deny_pattern(&mut self, pattern: &str) {
        if !self.path_deny_patterns.contains(&pattern.to_string()) {
            self.path_deny_patterns.push(pattern.to_string());
        }
    }

    /// Add path patterns from a list (daemon: protected_paths config).
    pub fn add_path_deny_patterns(&mut self, patterns: &[String]) {
        for p in patterns {
            self.add_path_deny_pattern(p);
        }
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
        // Also revoke network capabilities from the grant set
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

    fn check_path(&self, tool_name: &str, path: &Path) -> Option<PolicyDecision> {
        // Path traversal: must be within project_root for FsRead/FsWrite
        if let Ok(canonical) = path.canonicalize() {
            if let Ok(root) = self.project_root.canonicalize() {
                if !canonical.starts_with(&root) {
                    if !self.granted.has(Capability::FsReadExternal) {
                        return Some(PolicyDecision::Deny {
                            reason: format!(
                                "'{}' path {} is outside project root",
                                tool_name,
                                path.display()
                            ),
                        });
                    }
                }
            }
        }

        // Deny-list pattern check
        let path_str = path.display().to_string();
        for pattern in &self.path_deny_patterns {
            if simple_glob_match(pattern, &path_str) {
                return Some(PolicyDecision::Deny {
                    reason: format!(
                        "'{}' path {} matches deny pattern '{}'",
                        tool_name, path_str, pattern
                    ),
                });
            }
        }

        None
    }

    fn check_command(&self, tool_name: &str, cmd: &str) -> Option<PolicyDecision> {
        let cmd_lower = cmd.to_lowercase();
        for pattern in &self.command_deny_patterns {
            if cmd_lower.contains(&pattern.to_lowercase()) {
                return Some(PolicyDecision::Deny {
                    reason: format!(
                        "'{}' command matches deny pattern '{}': {}",
                        tool_name, pattern, cmd
                    ),
                });
            }
        }
        None
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
}
