//! New-generation tools built on the TypedTool foundation.
//!
//! Every tool here uses typed input (schemars), declared capabilities,
//! declared purity, and ToolCard self-description.

pub mod agent_tools;
pub mod meta;
pub mod notebook_repl;
pub mod plan_worktree;
pub mod scaffold;
pub mod schedule;
pub mod task;
pub mod web;

/// Register all typed tools into the registry.
pub fn register_all_typed_tools(registry: &mut crate::ToolRegistry) {
    // Phase 2: Task unification
    crate::register_typed(registry, task::UnifiedTaskTool::new());

    // Phase 3: Agent interaction
    crate::register_typed(registry, agent_tools::AskUserTool);

    // Phase 4: Plan mode + worktree
    crate::register_typed(registry, plan_worktree::PlanModeTool);
    crate::register_typed(registry, plan_worktree::WorktreeTool);

    // Phase 5: Web (already have WebFetchTool, add typed web_search)
    crate::register_typed(registry, web::TypedWebSearchTool);

    // Phase 7: Notebook + REPL
    crate::register_typed(registry, notebook_repl::NotebookTool);

    // Phase 9: Skills
    // (wired separately when pipit-skills is available)

    // Phase 10: Meta tools
    crate::register_typed(registry, meta::BriefContextTool);
    crate::register_typed(registry, meta::TypedToolSearchTool::new());
    crate::register_typed(registry, meta::ConfigAccessTool);
    crate::register_typed(registry, meta::SleepWaitTool);

    // Phase 11: Scheduled jobs
    crate::register_typed(registry, schedule::ScheduleTool::new());

    // Phase 12: Project scaffolding
    crate::register_typed(registry, scaffold::ScaffoldProjectTool);
}
