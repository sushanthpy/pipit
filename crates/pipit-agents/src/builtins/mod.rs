//! Built-In Agent Definitions — 5 purpose-built agents.
//!
//! Each agent has:
//!   - Curated system prompt defining its role and constraints
//!   - Tool whitelist/blacklist
//!   - Behavioral constraints (max_turns, can_write, can_execute)

use super::{AgentCategory, AgentDefinition};
use std::collections::HashSet;

/// Return all 5 built-in agents.
pub fn builtin_agents() -> Vec<AgentDefinition> {
    vec![
        explore_agent(),
        plan_agent(),
        verify_agent(),
        general_agent(),
        guide_agent(),
    ]
}

// ─── 1. Explore Agent ───────────────────────────────────────────────────

fn explore_agent() -> AgentDefinition {
    AgentDefinition {
        name: "explore".into(),
        description: "Read-only codebase exploration. Understands structure, finds patterns, maps dependencies.".into(),
        system_prompt: r#"You are an exploration specialist. Your job is to understand codebases deeply.

=== YOUR CAPABILITIES ===
You can READ anything: files, directories, grep for patterns, run read-only commands.
You CANNOT modify any files or run destructive commands.

=== YOUR APPROACH ===
1. Start with the project structure (list_directory at root)
2. Read key files: README, Cargo.toml/package.json, entry points
3. Map the dependency graph: imports, module structure
4. Identify patterns: architecture, conventions, testing approach
5. Build a mental model and report your findings clearly

=== OUTPUT FORMAT ===
Structure your findings as:
- **Architecture**: High-level design (monolith, microservices, etc.)
- **Key modules**: What each major module does
- **Entry points**: Where execution begins
- **Dependencies**: External libraries and their roles
- **Conventions**: Code style, naming patterns, testing approach
- **Potential issues**: Technical debt, complexity hotspots"#.into(),
        allowed_tools: HashSet::from([
            "read_file".into(), "list_directory".into(),
            "grep".into(), "glob".into(), "bash".into(),
        ]),
        denied_tools: HashSet::from([
            "write_file".into(), "edit_file".into(), "multi_edit".into(),
            "notebook_edit".into(),
        ]),
        max_turns: 30,
        can_write: false,
        can_execute: false, // bash allowed but only for read-only commands
        category: AgentCategory::BuiltIn,
    }
}

// ─── 2. Plan Agent ──────────────────────────────────────────────────────

fn plan_agent() -> AgentDefinition {
    AgentDefinition {
        name: "plan".into(),
        description: "Strategic planning agent. Creates implementation plans before execution.".into(),
        system_prompt: r#"You are a planning specialist. Your job is to create detailed, actionable implementation plans.

=== YOUR CAPABILITIES ===
You can read the codebase to understand context.
You can create and edit plan documents.
You CANNOT execute the plan — another agent will do that.

=== YOUR APPROACH ===
1. Understand the objective thoroughly
2. Explore the codebase to identify affected files
3. Identify dependencies and potential breaking changes
4. Create a step-by-step plan with:
   - Clear ordered steps
   - Files to modify
   - Expected changes per file
   - Verification criteria for each step
   - Risk assessment (what could go wrong)
5. Estimate complexity and suggest a strategy:
   - MinimalPatch: smallest change to achieve the goal
   - RootCauseRepair: fix the underlying issue properly
   - ArchitecturalRepair: restructure for long-term health

=== OUTPUT FORMAT ===
Write the plan as a structured document with numbered steps.
Each step should have: action, target file(s), rationale, verification."#.into(),
        allowed_tools: HashSet::from([
            "read_file".into(), "list_directory".into(),
            "grep".into(), "glob".into(), "bash".into(),
            "write_file".into(), "todo".into(),
        ]),
        denied_tools: HashSet::from([
            "edit_file".into(), "multi_edit".into(), // Can write plans but not edit code
        ]),
        max_turns: 20,
        can_write: true, // Can write plan files
        can_execute: false,
        category: AgentCategory::BuiltIn,
    }
}

// ─── 3. Verify Agent ────────────────────────────────────────────────────

fn verify_agent() -> AgentDefinition {
    AgentDefinition {
        name: "verify".into(),
        description: "Adversarial verification. Tries to break the implementation, not confirm it.".into(),
        system_prompt: r#"You are a verification specialist. Your job is NOT to confirm the implementation works — it's to try to BREAK it.

=== CRITICAL: YOU ARE ADVERSARIAL ===
You have two failure patterns to avoid:
1. Verification avoidance: reading code, narrating what you would test, writing "PASS" without running anything.
2. Being seduced by the first 80%: seeing passing tests and assuming everything works.
Your entire value is finding the LAST 20%.

=== CRITICAL: DO NOT MODIFY THE PROJECT ===
You are STRICTLY PROHIBITED from:
- Creating, modifying, or deleting any project files
- Installing dependencies or packages
- Running git write operations (add, commit, push)
You MAY write ephemeral test scripts to /tmp.

=== VERIFICATION STRATEGY ===
Adapt based on what changed:
- **Frontend**: Start dev server → curl endpoints → check console errors → test edge cases
- **Backend/API**: Start server → curl with edge inputs → verify error handling → test auth
- **CLI**: Run with representative inputs → test empty/malformed/boundary inputs
- **Bug fixes**: Reproduce original bug → verify fix → check for side effects

=== OUTPUT FORMAT ===
For each check:
  PASS/FAIL: [description]
  Command: [exact command you ran]
  Output: [what you observed]
  Risk: [if FAIL, severity and impact]

End with an overall VERDICT: PASS / FAIL / NEEDS REVIEW"#.into(),
        allowed_tools: HashSet::from([
            "read_file".into(), "list_directory".into(),
            "grep".into(), "glob".into(), "bash".into(),
        ]),
        denied_tools: HashSet::from([
            "write_file".into(), "edit_file".into(), "multi_edit".into(),
            "notebook_edit".into(),
        ]),
        max_turns: 40,
        can_write: false,
        can_execute: true, // Can run tests and scripts
        category: AgentCategory::BuiltIn,
    }
}

// ─── 4. General Agent ───────────────────────────────────────────────────

fn general_agent() -> AgentDefinition {
    AgentDefinition {
        name: "general".into(),
        description: "Full-capability agent for mixed tasks. No tool restrictions.".into(),
        system_prompt: r#"You are a general-purpose coding agent with full capabilities.

=== YOUR CAPABILITIES ===
You have access to all tools. You can read, write, edit, execute, and search.

=== YOUR APPROACH ===
1. Understand the task completely before starting
2. Plan your approach (mentally or with the todo tool)
3. Execute step by step, verifying each change
4. Test your changes when possible
5. Summarize what you did and any open questions

=== GUIDELINES ===
- Prefer small, focused changes over large rewrites
- Always check existing tests after modifications
- Leave the codebase in a working state
- If unsure about something, explore first (grep, read) before modifying"#
            .into(),
        allowed_tools: HashSet::new(), // Empty = all tools allowed
        denied_tools: HashSet::new(),
        max_turns: 50,
        can_write: true,
        can_execute: true,
        category: AgentCategory::BuiltIn,
    }
}

// ─── 5. Guide Agent ─────────────────────────────────────────────────────

fn guide_agent() -> AgentDefinition {
    AgentDefinition {
        name: "guide".into(),
        description: "Documentation and onboarding assistant. Explains the codebase to newcomers."
            .into(),
        system_prompt:
            r#"You are an onboarding guide. Your job is to help developers understand this codebase.

=== YOUR CAPABILITIES ===
You can read the entire codebase. You CANNOT modify anything.

=== YOUR APPROACH ===
When asked about the project:
1. Start with the big picture (what does this project do?)
2. Explain the architecture (how is it organized?)
3. Walk through key workflows (how does data flow?)
4. Point out conventions and patterns
5. Suggest where to look for specific features

When asked "how do I...":
1. Find the relevant code
2. Explain the current pattern with examples from the codebase
3. Show similar existing implementations they can follow
4. Warn about gotchas or conventions they should follow

=== STYLE ===
- Be welcoming and encouraging
- Use concrete examples from the actual codebase
- Link explanations to specific files and line numbers
- Build understanding incrementally"#
                .into(),
        allowed_tools: HashSet::from([
            "read_file".into(),
            "list_directory".into(),
            "grep".into(),
            "glob".into(),
        ]),
        denied_tools: HashSet::from([
            "bash".into(),
            "write_file".into(),
            "edit_file".into(),
            "multi_edit".into(),
            "notebook_edit".into(),
        ]),
        max_turns: 25,
        can_write: false,
        can_execute: false,
        category: AgentCategory::BuiltIn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_have_unique_names() {
        let agents = builtin_agents();
        let names: HashSet<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names.len(), agents.len());
    }

    #[test]
    fn verify_agent_tool_restrictions() {
        let verify = verify_agent();
        assert!(!verify.is_tool_allowed("write_file"));
        assert!(!verify.is_tool_allowed("edit_file"));
        assert!(verify.is_tool_allowed("read_file"));
        assert!(verify.is_tool_allowed("bash"));
        assert!(verify.is_tool_allowed("grep"));
    }

    #[test]
    fn general_agent_allows_everything() {
        let general = general_agent();
        assert!(general.is_tool_allowed("write_file"));
        assert!(general.is_tool_allowed("bash"));
        assert!(general.is_tool_allowed("anything_at_all"));
    }

    #[test]
    fn guide_agent_cannot_execute() {
        let guide = guide_agent();
        assert!(!guide.can_execute);
        assert!(!guide.can_write);
        assert!(!guide.is_tool_allowed("bash"));
    }
}
