---
description: "Use when shaping coding-agent workflow, planning complex implementation work, enforcing verification discipline, or turning recurring execution patterns into reusable CLI operating rules"
---
# Coding CLI Playbook Skill

Use this skill when the task is about how the coding CLI should operate, not just what product code should change.

Checklist:
- start with a brief plan for non-trivial tasks, including verification steps
- re-plan when repeated evidence shows the current approach is weak
- verify before declaring done; prefer tests, builds, runtime checks, or documented examples
- keep solutions simple and root-cause oriented
- use subagents or separate workflow assets when complexity or context isolation justifies them
- store reusable behavior in skills, hooks, prompts, instructions, or templates instead of repeating it ad hoc
- treat the file system as a context engine: organize references, scripts, templates, and workflow assets so they improve reasoning quality
- avoid micromanaging the model when clear goals and constraints are enough

When updating the workflow:
- prefer shared workspace instructions for always-on rules
- use a skill for repeatable task flows
- use hooks only for deterministic enforcement points
- add or update repo memory when a workflow lesson is likely to recur

Task: $ARGUMENTS
