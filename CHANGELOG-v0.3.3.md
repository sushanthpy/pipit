# pipit v0.3.3 — Profiling-Driven Quality Overhaul

**Date:** April 12, 2026  
**66 files changed, +11,787 −1,119**

---

## Summary

v0.3.3 is a quality-focused release driven by empirical request-payload profiling. The headline fix: a triple-cascade truncation bug was silently destroying **75% of file content** before the LLM ever saw it. Fixing this single root cause jumped bug-detection accuracy from 9/20 to 20/20 on a synthetic 10K-line codebase — surpassing the pi reference agent (19/20).

---

## Critical Fix: Tool Result Truncation

### Problem
Three stacked truncation layers in `push_tool_result()` were compounding:

| Layer | Old Threshold | Effect |
|-------|--------------|--------|
| `head_tail_split` | 200 lines | Keep 100 head + 100 tail |
| `micro_compact` | 2 KB | Keep 50 head + 50 tail |
| `tool_result_max_chars` | 32 KB | Hard char cutoff |

A 743-line file (database.py, 33KB) → only **3,983 bytes delivered** (11.8%).  
Total across 13 source files: 202KB actual → 52KB delivered (25%).

### Discovery
Added `PIPIT_DUMP_REQUESTS` env var to the OpenAI provider. When set to a directory path, pipit writes full request JSON + per-component byte summaries for every LLM call. Comparing pipit's payloads against pi's session log revealed the content loss.

### Fix
| Parameter | Before | After |
|-----------|--------|-------|
| `HEAD_TAIL_MAX_LINES` | 200 | 2,000 |
| `MICRO_COMPACT_THRESHOLD` | 2 KB | 64 KB |
| `KEEP_HEAD/TAIL_LINES` | 50 | 500 |
| `tool_result_max_chars` | 32 KB | 128 KB |

**Files:** `crates/pipit-context/src/budget.rs`, `crates/pipit-cli/src/main.rs`

### Result
| Version | Bugs Found | Turns | File Content Delivered |
|---------|-----------|-------|----------------------|
| v0.3.2 (before) | 5/20 | 5 | 52 KB (25%) |
| v0.3.3 (arch fixes) | 9/20 | 5 | 52 KB (25%) |
| **v0.3.3 (truncation fix)** | **20/20** | **3** | **250 KB (100%)** |
| pi reference | 19/20 | ~7 | ~213 KB |

---

## Agent Behavior Improvements

### read_file Truncation with Continuation Hints
- Per-read cap: 2,000 lines / 50KB
- Adds `[Showing lines X-Y of TOTAL. Use start_line=N to continue.]` hint
- **File:** `crates/pipit-tools/src/builtins/read_file.rs`

### Smarter Auto-Continue Logic
- `response_looks_like_summary`: removed `len > 80` heuristic that false-triggered on analysis tasks
- Added `analysis_is_final`: recognizes >200-char analysis responses as complete when no mutations occurred
- Nag prompt simplified from verbose instructions to just `"Continue."`
- **File:** `crates/pipit-core/src/agent.rs`

### Context Preservation
- Eviction age threshold: 6 → 20 messages (stops destroying file contents mid-analysis)
- TTFT compaction threshold: 5s → 30s (local models are slow; 5s was triggering premature eviction)
- **Files:** `crates/pipit-core/src/agent.rs`, `crates/pipit-core/src/query_profiler.rs`

### Prompt & Token Optimization
- Compact system prompt: removed tool descriptions already present in API tool schemas
- Added "Batch independent tool calls" guideline
- RepoMap `BYTES_PER_TOKEN`: 4 → 3 (renders within budget)
- Subagent tool description: 5,161 → 2,568 bytes (removed verbose examples)
- **Files:** `crates/pipit-core/src/prompt_kernel.rs`, `crates/pipit-intelligence/src/repomap.rs`, `crates/pipit-tools/src/builtins/subagent/mod.rs`

### Provider Fixes
- `openai_compatible` default `context_window`: 200K → 32K (safe default when unconfigured)
- llama.cpp `context size exceeded` error detection
- **Files:** `crates/pipit-config/src/lib.rs`, `crates/pipit-core/src/agent.rs`

---

## TUI Enhancements

### Code Display for File Writes
When pipit writes a file, the content pane now shows the full code in a syntax-highlighted fenced code block (```js, ```py, etc.) — up to 200 lines. Previously only showed "Wrote file.js (12 lines)".  
**File:** `crates/pipit-cli/src/tui.rs`

### Inline Result Previews
`read_file`, `grep`, and `list_directory` results now show an 8-line preview in the content pane, so you can see what the agent is reading.  
**File:** `crates/pipit-cli/src/tui.rs`

### Paste-as-Attachment
- Long pastes (>100 chars or multi-line) are saved to a temp file and displayed as a `📋 pasted text ×` chip
- Short pastes (≤100 chars, single line) still insert inline
- New `PastedText` attachment kind with yellow styling
- Image path auto-extraction now requires `Path::exists()` to prevent false matches from random pasted text
- **File:** `crates/pipit-io/src/composer.rs`

---

## New Crates & Modules

| Crate/Module | Purpose |
|-------------|---------|
| `pipit-vim` | Modal editing engine with Vim keybindings for the composer |
| `pipit-tmux` | Terminal multiplexer integration |
| `pipit-agents` | Agent catalog system (`catalog.rs`, `catalog_v2.rs`) |
| `pipit-core/domain_architect.rs` | Domain-aware architecture analysis |
| `pipit-core/integration_verifier.rs` | Integration verification |
| `pipit-config/provider_roster.rs` | Provider configuration management |
| `pipit-tools/builtins/subagent/` | Refactored from single file to module with parallel, chain, and fork execution modes |
| `pipit-tools/builtins/typed/todo.rs` | Typed todo/task tool |
| `pipit-tools/prompts/` | Prompt templates for tools |

---

## Profiling Infrastructure

### `PIPIT_DUMP_REQUESTS`
Set `PIPIT_DUMP_REQUESTS=/path/to/dir` to capture every LLM request:
- `req_NNNN.json` — full request body
- `req_NNNN_summary.txt` — per-component byte breakdown (system prompt, each message, tool definitions)

Useful for diagnosing token allocation, context waste, and comparing with other agents.  
**File:** `crates/pipit-provider/src/openai.rs`
