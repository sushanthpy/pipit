# Pipit TUI — Real-World Test Results (April 2026 Redesign)

**Date:** April 6, 2026  
**Binary:** `target/debug/pipit` (v0.2.3)  
**Test dir:** `/tmp/pipit-test` (git repo with single README.md)

---

## Root Cause of "Commands Don't Work"

The redesign introduced a **two-mode architecture** (Shell + Task) where:
- **Shell mode** only shows the composer + hints. No content_lines or activity_lines are rendered.
- **Task mode** shows activity feed + response content.

**The bug:** Every local command handler (`/help`, `/diff`, `!ls`, etc.) pushed output to `content_lines` or `activity_lines` but **did not switch to Task mode**, so the output was written but never displayed.

**Fix applied:** Every command handler that produces visible output now sets `s.ui_mode = UiMode::Task` so the user immediately sees the result.

---

## Command-by-Command Test Matrix

### Local Commands (handled entirely in tui.rs)

| Command | Produces Output | Switches to Task | Status | Notes |
|---------|----------------|-------------------|--------|-------|
| `/help` | content_lines (100+ lines) | Yes | FIXED | Shows full help in content pane |
| `/quit` `/q` | none | N/A | Works | Exits cleanly |
| `/clear` | clears everything | Returns to Shell | Works | Resets state, goes back to Shell |
| `/cost` | activity_lines | Yes | FIXED | Shows cost summary in activity |
| `/status` | activity_lines | Yes | FIXED | Shows repo/model/tokens info |
| `/config` | content_lines (~20 lines) | Yes | FIXED | Shows config file path and settings |
| `/setup` | content_lines (~13 lines) | Yes | FIXED | Shows setup wizard instructions |
| `/doctor` | content_lines (~20 lines) | Yes | FIXED | Shows health check report |
| `/diff` | content_lines (git diff) | Yes | FIXED | Shows staged + unstaged changes |
| `/branches` | content_lines (branch list) | Yes | FIXED | Lists all git branches |
| `/branch` | activity_lines | Yes | FIXED | Shows/creates branch |
| `/switch <b>` | activity_lines | Yes | FIXED | Switches git branch |
| `/undo` | activity_lines | Yes | FIXED | Rolls back files via git |
| `!command` | content_lines (stdout/stderr) | Yes | FIXED | Runs shell command directly |

### Agent-Delegated Commands (sent to agent via prompt_tx)

| Command | Route | Status | Notes |
|---------|-------|--------|-------|
| `/skills` | prompt_tx.send("/skills") | Works | Agent handles; auto-switches to Task |
| `/hooks` | prompt_tx.send("/hooks") | Works | Agent handles |
| `/mcp` | prompt_tx.send("/mcp") | Works | Agent handles |
| `/plan`, `/verify`, `/tdd`, etc. | prompt_tx.send("/cmd") | Works | Catch-all routes to agent |
| Regular text prompt | prompt_tx.send(text) | Works | Agent auto-switches to Task mode |
| @file prompt | enriched + prompt_tx.send(...) | Works | File context attached |

### Input Types

| Input | Handler | Status | Notes |
|-------|---------|--------|-------|
| Regular text | Prompt -> agent | Works | Switches to Task when agent starts |
| /cmd | Command -> local or agent | Works | See per-command table above |
| !cmd | ShellPassthrough -> tokio exec | Fixed | Uses async process, shows output |
| @file text | PromptWithFiles -> agent | Works | Enriches prompt with file refs |
| ? | Maps to /help | Works | Shortcut for help |

---

## Bugs Found and Fixed

### Bug 1: Commands produce invisible output
- **Root cause:** content_lines and activity_lines only rendered in Task mode, but commands didn't switch modes
- **Fix:** Added s.ui_mode = UiMode::Task to all 14 local command handlers

### Bug 2: Agent completion returns to blank Shell
- **Root cause:** s.ui_mode = UiMode::Shell was set on agent completion, hiding results
- **Fix:** Removed auto-switch to Shell. User presses g to return manually.

### Bug 3: Task label only set on first input
- **Root cause:** task_label was only set inside if !state.has_received_input block
- **Fix:** Now updates task_label on every submission

### Bug 4: Top bar missing app name/version
- **Root cause:** Top bar showed repo name but not pipit branding
- **Fix:** Added pipit v0.2.3 badge to top bar left side

---

## Known Remaining Issues

### Minor
1. /cost and /status show results in activity feed only (1 line). Could be richer.
2. /undo uses blocking std::process::Command in async context.
3. /diff truncates at 200 lines per section silently.
4. /config [key] ignores the key parameter.

### Structural
5. Agent-delegated commands rely on the agent task setting UiMode::Task when it starts working. If the agent fails to start (no API key), user stays in Shell with no feedback.
6. The g key to return to Shell only works when the composer is empty.

---

## Keyboard Controls

| Key | Context | Action |
|-----|---------|--------|
| Enter | Shell/Task | Submit input |
| Esc | Working | Cancel current agent run |
| Ctrl-C | Any | Quit |
| g | Task (empty composer) | Return to Shell mode |
| Alt-Up | Task | Scroll content up |
| Alt-Down | Task | Scroll content down |
| Ctrl-J | Any | Insert newline (multiline) |
| Tab | Any | Trigger/cycle completion |
| Up/Down | Shell (empty) | History recall |

---

## Mode Transitions

```
Shell --[submit prompt]--> Task (agent starts)
Shell --[/help, /diff, !cmd, etc.]--> Task (output visible)
Shell --[/clear]--> Shell (reset)
Task --[g key]--> Shell
Task --[agent finishes]--> Task (stays, shows results)
```
