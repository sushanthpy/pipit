---
description: "Use when reviewing pull requests for code quality and style issues"
disable-model-invocation: false
user-invocable: true
allowed-tools:
  - read_file
  - grep_search
  - file_search
agent:
  model: "gpt-4"
  max_turns: 5
when-to-use: "When the user asks for a code review or PR review"
argument-hint: "PR number or file path"
model: "gpt-4o"
effort: "high"
---
# Code Review Skill

Review the code changes for quality, style, and correctness.

Task: $ARGUMENTS
