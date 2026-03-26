# Pipit Coding CLI Playbook

This workspace keeps reusable workflow assets under `.github/` and `.pipit/`, and expects the coding CLI to follow a proof-oriented, plan-first workflow.

## Shared Layout

- `.github/skills/` stores project-level skills that should be easy to discover in chat.
- `.github/hooks/` stores deterministic hook manifests for workflow enforcement.
- `.github/mcp.json` stores example MCP server wiring for local development.
- `.pipit/skills/` continues to hold Pipit-native skills that should be discovered by the CLI.

## Default Workflow

- Enter plan mode for any non-trivial task with multiple steps, architectural impact, or unclear failure modes.
- Define both execution steps and verification steps before making broad changes.
- If evidence shows the current approach is wrong, stop and re-plan instead of pushing the same fix harder.
- Prefer explicit acceptance criteria, documented examples, and deterministic checks over vague completion claims.

## Verification Before Done

- Never mark work done without proof.
- Run targeted tests, builds, runtime checks, or documented examples whenever the repo makes that possible.
- Compare expected versus actual behavior, not just whether a command exited successfully.
- Before finishing, ask whether a skeptical senior engineer would accept the evidence.

## Subagent And Context Strategy

- Use subagents aggressively for complex research, codebase exploration, or isolated analysis tasks.
- Keep the main thread focused on execution decisions and user-visible progress.
- Treat the file system as a context engine: prefer structured folders for references, scripts, templates, workflows, and reusable assets.
- Use progressive disclosure: load the smallest relevant instruction or skill before widening scope.

## Implementation Principles

- Simplicity first: prefer minimal, clean solutions over clever ones.
- Solve root causes instead of stacking superficial patches.
- Avoid over-constraining the model with micromanagement when a clear objective and constraints are enough.
- Track mistakes, convert them into reusable rules, and update shared workflow assets when a pattern is likely to recur.
- Use skills for repeatable workflows such as verification, automation, scaffolding, and data analysis.

## Expectations

- Prefer small, composable workflow assets over one giant instruction file.
- Keep descriptions explicit so the agent can discover the right skill.
- Treat hook and MCP manifests as reviewed project assets, not ad hoc personal config.
- Keep shared instructions durable and repo-relevant; task-specific detail belongs in skills or prompts.