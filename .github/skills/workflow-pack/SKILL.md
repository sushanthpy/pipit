---
description: "Use when setting up or reviewing workspace workflow assets such as skills, hooks, MCP manifests, and shared agent instructions"
---
# Workflow Pack Skill

Use this skill when the task is about project workflow setup rather than product code.

Checklist:
- confirm the right scope for shared instructions versus task-specific skills
- inspect `.github/hooks` and `.github/mcp.json` before proposing new workflow files
- keep descriptions explicit so the agent can discover the right asset
- prefer the smallest workflow change that makes behavior more consistent

Task: $ARGUMENTS