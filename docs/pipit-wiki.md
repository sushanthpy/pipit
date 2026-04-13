# Pipit — Architecture Wiki

> **Version:** 0.2.3 · **Rust Edition:** 2024 · **MSRV:** 1.90  
> **Codebase:** 96,820 lines of Rust across 34 crates · 569 unit tests + 4 integration test suites  
> **License:** MIT

---

## Table of Contents

1. [System Overview](#1-system-overview)
2. [Crate Map](#2-crate-map)
3. [Agent Loop & Turn State Machine](#3-agent-loop--turn-state-machine)
4. [Tool System](#4-tool-system)
5. [LLM Provider Layer](#5-llm-provider-layer)
6. [Permission & Safety Model](#6-permission--safety-model)
7. [Context Management & Compaction](#7-context-management--compaction)
8. [Session Persistence & Crash Recovery](#8-session-persistence--crash-recovery)
9. [Planning, Verification & Proof](#9-planning-verification--proof)
10. [Extensibility & Hooks](#10-extensibility--hooks)
11. [Delegation & Multi-Agent](#11-delegation--multi-agent)
12. [Mesh Networking](#12-mesh-networking)
13. [TUI Architecture](#13-tui-architecture)
14. [Configuration System](#14-configuration-system)
15. [Code Intelligence](#15-code-intelligence)
16. [Daemon & Background Tasks](#16-daemon--background-tasks)
17. [Testing Philosophy](#17-testing-philosophy)
18. [Mathematical Invariants](#18-mathematical-invariants)
19. [CLI Reference](#19-cli-reference)
20. [Glossary](#20-glossary)

---

## 1. System Overview

Pipit is a terminal-native AI coding agent. It coordinates LLM inference, tool execution, context management, and session persistence through a formally structured turn loop. The architecture is modular — 34 independent crates communicate through typed interfaces — but the runtime is centralized: every mode (CLI, TUI, JSON/headless, daemon, SDK) converges on the same `AgentLoop::run()` method.

### Core Architecture Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                      Frontends                              │
│   CLI (pipit-cli)  │  TUI (pipit-io)  │  SDK (pipit-core)  │
└────────┬───────────┴───────┬───────────┴────────┬───────────┘
         │                   │                    │
         ▼                   ▼                    ▼
┌─────────────────────────────────────────────────────────────┐
│                   AgentLoop::run()                          │
│   TurnKernel (FSM) · PolicyKernel · SessionKernel           │
│   ProofState · TelemetryFacade · LoopDetector               │
├─────────────────────────────────────────────────────────────┤
│  ModelRouter     │  ToolRegistry    │  ExtensionRunner       │
│  (pipit-provider)│  (pipit-tools)   │  (pipit-extensions)    │
├─────────────────────────────────────────────────────────────┤
│  ContextManager  │  SessionLedger   │  CompactionPipeline    │
│  (pipit-context) │  (pipit-core)    │  (pipit-context)       │
└─────────────────────────────────────────────────────────────┘
```

### Key Design Principles

1. **Single authority per concern.** PolicyKernel is the sole authorization oracle. SessionKernel is the sole state mutation authority. TurnKernel is the sole phase governor.
2. **Event-sourced persistence.** All state mutations flow through the SessionLedger as a hash-chained, append-only event log. Recovery is replay.
3. **Typed tool semantics.** Every tool declares its purity, capabilities, and resource signature as compile-time constants, not runtime strings.
4. **Deterministic resume.** The hydration protocol guarantees `resume(execute(S)) ≈ S` by traversing a fixed dependency DAG in the same topological order as live execution.

---

## 2. Crate Map

### By Function (lines of Rust)

| Role | Crate | LOC | Purpose |
|------|-------|-----|---------|
| **Runtime Core** | pipit-core | 25,616 | Agent loop, turn FSM, kernels, proof, scheduling |
| **Tool System** | pipit-tools | 9,677 | 30 built-in tools, typed tool foundation, MCP |
| **Terminal UI** | pipit-io | 9,859 | Two-mode TUI (Shell + Task), composer, syntax highlighting |
| **Background** | pipit-daemon | 8,669 | HTTP API, task pool, git integration |
| **CLI** | pipit-cli | 5,492 | Argument parsing, REPL, slash commands, auth |
| **Context** | pipit-context | 4,868 | Token budget, 6-stage compaction, dedup, utility scoring |
| **Provider** | pipit-provider | 3,557 | LLM abstraction for 13 providers |
| **Mesh** | pipit-mesh | 3,400 | SWIM gossip, CRDT, phi-accrual, capability routing, agent discovery |
| **Intelligence** | pipit-intelligence | 2,560 | RepoMap, semantic code analysis |
| **Permissions** | pipit-permissions | 2,302 | Lattice classifiers, symlink detection |
| **Extensions** | pipit-extensions | 2,133 | 5-variant hook system, bitmask events, WASM sandbox |
| **Memory** | pipit-memory | 1,997 | Persistent MEMORY.md, secret scanning, team sync |
| **Skills** | pipit-skills | 1,770 | Skill registry, HMAC signing, budget enforcement |
| **Config** | pipit-config | 1,645 | Layered config (system→user→project→env→CLI) |
| **Browser** | pipit-browser | 1,491 | Headless Chrome CDP, visual regression |
| **MCP** | pipit-mcp | 1,148 | MCP client (stdio + SSE), A2A protocol |
| **Voice** | pipit-voice | 1,077 | VAD, transcription, speech-to-text |
| **Edit** | pipit-edit | 874 | Search/replace, unified diff, whole-file formats |

### Dependency Graph (Core Path)

```
pipit-cli ──► pipit-core ──► pipit-provider
    │              │
    │              ├──► pipit-tools ──► pipit-edit
    │              ├──► pipit-context
    │              ├──► pipit-extensions
    │              ├──► pipit-intelligence
    │              ├──► pipit-memory / verify / permissions / env / ...
    │
    ├──► pipit-io (TUI)
    ├──► pipit-skills / mcp / bench / browser / deps / voice / mesh
    └──► pipit-daemon (background)
```

---

## 3. Agent Loop & Turn State Machine

### The Canonical Turn FSM

Every turn traverses a deterministic phase sequence enforced by `TurnKernel`, a pure Mealy machine with no I/O:

```
Idle → Accepted → ContextFrozen → Requesting → ResponseStarted →
  ├→ ResponseCompleted → Committed  (no tools)
  └→ ToolProposed → [PermissionResolved → ToolStarted → ToolCompleted] →
     └→ Verifying → Requesting  (loop for next LLM call)
```

**13 phases.** Terminal: Committed (success), Failed (error/cancel). Transition validation O(1) per event.

### The Agent Loop

Located in `pipit-core/src/agent.rs` (~1,600 lines). This is the central execution path used by CLI, TUI, SDK, and daemon:

```
1. Preprocess input through extensions
2. Create Objective from prompt
3. Record UserMessageAccepted (mandatory persistence boundary)
4. Run heuristic planner → select strategy
5. Initialize ProofState
6. FSM: Idle → Accepted → ContextFrozen
7. LOOP:
   a. Drain steering messages
   b. Grace period / cost budget check
   c. FSM: RequestSent → LLM call → StreamStarted → ResponseStarted
   d. Cost tracking (Kahan summation) + telemetry feedback (TTFT EMA)
   e. MATCH stop_reason:
      - EndTurn: → ResponseCompleted → TurnCommitted → yield
      - ToolUse: → ToolProposed → execute_tools → AllToolsCompleted → loop
      - MaxTokens: inject "Continue" → loop
```

### Grace Period

Instead of hard-stopping at `max_turns`, grants 3 bonus turns if recent forward progress (mutation in last 3 turns). 120-second wall-clock timeout prevents infinite grace.

### Loop Detection

1. **Tool repetition:** Tracks `(tool_name, args_hash)` over sliding window. Fires at 3 identical consecutive calls.
2. **Semantic loop:** Cosine similarity of reasoning text. Fires at >70% similarity across 3 turns.

---

## 4. Tool System

### 30 Registered Tools

**Core (8):** read_file, write_file, edit_file, multi_edit, list_directory, grep, glob, bash  
**Analysis (4):** symbol_xref, change_impact, test_selector, api_surface  
**Extended (7):** web_fetch, web_search, powershell, repl, skill, lsp, remote_trigger  
**Typed (11):** task, ask_user, plan_mode, worktree, web_search_typed, notebook, brief, tool_search, config, sleep, schedule

### TypedTool Foundation

```rust
trait TypedTool: Send + Sync + 'static {
    type Input: DeserializeOwned + JsonSchema + Send;
    const NAME: &'static str;
    const CAPABILITIES: CapabilitySet;
    const PURITY: Purity;
    fn describe() -> ToolCard;
    async fn execute(input, ctx, cancel) -> Result<TypedToolResult, ToolError>;
}
```

`TypedToolAdapter<T>` bridges into legacy `Tool` trait. Schema auto-generated via `schemars`. Registration via `register_typed(registry, tool)`.

### Tool Execution Pipeline

```
SemanticClass → PolicyKernel::evaluate() → Scheduler::batch() → Execute → Evidence → ProofState
```

Scheduler creates maximal independent sets: concurrent reads, sequential writes.

### Bash Safety (3 Layers)

1. **Lexical:** Block `rm -rf /`, `mkfs`, `dd if=`, fork bombs. Normalize against encoding tricks.
2. **Allowlist:** Configurable binary allowlist via `.pipit/sandbox.toml`.
3. **Kernel sandbox:** macOS seatbelt / Linux bwrap. Uses `bash -c` (not `sh`) for brace expansion.

Non-zero exit returns descriptive content (not `Err`) so the model gets diagnostic info.

---

## 5. LLM Provider Layer

13 providers: Anthropic, OpenAI, Azure, Gemini, Vertex, DeepSeek, OpenRouter, XAI, Cerebras, Groq, Mistral, Ollama, OpenAI-compatible.

```rust
trait LlmProvider: Send + Sync {
    fn id() -> &str;
    async fn complete(request, cancel) -> Stream<ContentEvent>;
    async fn count_tokens(messages) -> TokenCount;
    fn capabilities() -> &ModelCapabilities;
    fn supports_cache_edit() -> bool { false }  // Anthropic cache protocol
    async fn edit_cache(edits) -> CacheEditReceipt { ... }
}
```

`ModelRouter` routes by role (Planner/Executor/Verifier) for PEV mode.

---

## 6. Permission & Safety Model

### PolicyKernel

13 capabilities as u32 bitset: `FsRead | FsWrite | ProcessExec | NetworkRead | McpInvoke | Delegate | ...`

Decision types: Allow, Ask, Deny, Sandbox. Check order: tool overrides → deny list → lattice R⊆G → resource scopes → zone policy → subagent depth.

### Approval Modes

| Mode | Behavior |
|------|----------|
| `suggest` | Everything requires approval |
| `auto_edit` | Edits auto, shell requires |
| `command_review` | Shell requires, reads don't |
| `full_auto` | No prompts in trusted folders |

### Permission Classifiers

DangerousCommand, Symlink, Docker, GitRemote, FileType, SedMutation — each implemented as a `Classifier` trait with pattern matching and path analysis.

---

## 7. Context Management & Compaction

### Six-Stage Pipeline

| Stage | Pass | Complexity | Function |
|-------|------|-----------|----------|
| 0 | DedupCompactPass | O(n) | Content-addressed dedup of duplicate tool results |
| 1 | ToolResultBudgetPass | O(n) | Truncate oversized outputs |
| 2 | SnipCompactPass | O(n) | Boundary-based truncation |
| 3 | MicroCompactPass | O(n) | Remove stale tool results |
| 4 | ContextCollapsePass | O(n) | Commit staged collapses |
| 5 | AutoCompactPass | O(LLM) | LLM-based summarization |

### Utility-Based Eviction

Messages scored by: `u = e^{-λd} × w_role × s_size`. Greedy knapsack: sort by u/c, select until budget. O(n log n).

### Speculative Compaction

Runs dedup+knapsack parallel with LLM call. Committed only on context overflow.

### Session Memory Sink

`MemoryStore` trait: store/recall/list/delete. AutoCompactPass summaries persisted for long-term retrieval.

---

## 8. Session Persistence & Crash Recovery

### SessionKernel = Ledger + WAL + PermissionLedger + DerivedState

Hash-chained event log (24 event types). 6 mandatory persistence boundaries: UserAccepted, ResponseBegin, ToolProposed, PermissionResolved, ToolCompleted, TurnCommitted.

### Hydration: `Ledger → Context → Worktree → Permissions → UI`

**Invariant:** `resume(execute(S)) ≈ S`

CLI: `pipit --resume` hydrates from `.pipit/sessions/latest/`.

8 crash-recovery conformance tests prove the invariant at every boundary.

---

## 9. Planning, Verification & Proof

### Strategies

MinimalPatch, RootCauseRepair, CharacterizationFirst, ArchitecturalRepair, DiagnosticOnly. Each with `PlanSource` provenance (Heuristic/LlmStructured/UserSpecified).

### 5-Factor Confidence

root_cause, semantic_understanding, side_effect_risk, verification_strength, environment_certainty → averaged.

### ProofPacket

Terminal artifact: objective + plan + pivots + evidence + edits + risk + confidence + rollback checkpoint. Surfaced in JSON output as `proof` object.

### PEV Modes

Fast (no verify), Balanced (heuristic + 1 repair), Guarded (LLM verify + 2 repairs).

---

## 10. Extensibility & Hooks

### 5 Hook Mediums (Algebraically Closed)

Command, Prompt, Http, Agent, Wasm. Adding a new variant is a compile error until all runtimes handle it.

### Event Bitmask Lattice

27 events as u64 bits. 8 category masks. Subscription: `mask ∧ event ≠ ⊥`.

### WASM Sandbox

wasmtime with fuel metering. WCET ≤ fuel_limit / min_instr_cost. SHA-256 module cache.

### Hook Replay

Content-addressed decision cache. Replay mode returns cached decisions without re-executing.

---

## 11. Delegation & Multi-Agent

Decision boundary: `E[V_delegate] > E[V_local] + λ·L_d`.

Capability inheritance: `child_cap = grant ∩ request` (lattice meet).

SubagentTool: in-process or worktree-isolated with merge contract validation.

---

## 12. Mesh Networking

**pipit-mesh:** SWIM gossip, phi-accrual failure detection, CRDT state, mDNS discovery, DashMap registry, cosine-similarity capability discovery, schema negotiation via lattice meet.

MeshDelegation: affinity-based node selection, TCP task dispatch.

---

## 13. TUI Architecture

Two modes: Shell (prompt + hints) and Task (header + activity + response + status + composer).

Auto-switch Shell→Task on agent start. `g` returns to Shell.

Live streaming cursor `▌` at end of response text. Status bar shows spinner + status + active tool + turn counter + elapsed time.

---

## 14. Configuration

Resolution: compiled defaults → /etc/pipit → ~/.config/pipit → .pipit/ → PIPIT_* env → CLI flags.

PIPIT.md auto-injected into system prompt.

---

## 15. Code Intelligence

RepoMap via Tree-sitter (Rust/Python/JS/TS/Go). Per-file mtime invalidation cache. Content-aware token estimation.

---

## 16. Daemon

HTTP API at `POST /api/tasks`. Each task gets own AgentLoop + SessionKernel. Git integration for automated commits.

---

## 17. Testing

314 tests: 26 FSM conformance + 8 crash recovery + 164 core + 19 benchmark + 32 context + 41 extensions + 24 tools.

---

## 18. Mathematical Invariants

| Invariant | Formula |
|-----------|---------|
| Capability meet | child = grant ∩ request |
| Hydration idempotence | resume(execute(S)) = S |
| DAG order | ∀(d,n)∈E: pos(d) < pos(n) |
| Delegation | E[Vd] > E[Vl] + λ·L |
| Knapsack | argmax Σu s.t. Σc ≤ B |
| Pipeline monotonicity | \|f(M)\| ≤ \|M\| |
| Event lattice | (H, ∨, ∧, ⊥, ⊤) bounded distributive |
| Replay | replay(record(s,e)) = apply(s,e) |
| WASM bound | WCET ≤ fuel/min_cost |
| Risk floor | max(factor, 0.1) |

---

## 19. CLI Reference

```bash
pipit "fix the bug"                           # Non-interactive
pipit                                          # Interactive TUI
pipit --provider openai --model gpt-4o "task"  # Provider override
pipit --approval full_auto "run tests"         # Auto mode
pipit --mode guarded "risky refactor"          # Full PEV
pipit --resume                                 # Resume last session
pipit --json "task" | jq '.proof'              # CI/scripting
pipit --max-turns 20 "complex task"            # Turn limit
pipit --classic                                # REPL mode
pipit auth login anthropic                     # Auth
pipit setup                                    # Setup wizard
```

---

## 20. Glossary

| Term | Definition |
|------|-----------|
| AgentLoop | Central execution loop (LLM calls + tools + state) |
| CapabilitySet | u32 bitmask of 13 permissions |
| CompactionPipeline | 6-stage token reduction chain |
| DedupPass | Content-addressed duplicate tool result elimination |
| HookKind | Closed enum: Command/Prompt/Http/Agent/Wasm |
| HookEventMask | u64 lattice for event subscription |
| MandatoryBoundary | 6 persistence cut points per turn |
| ModelRouter | Role-based LLM routing (Planner/Executor/Verifier) |
| PEV | Plan/Execute/Verify orchestration |
| PolicyKernel | Single authorization oracle |
| ProofPacket | Terminal evidence artifact |
| ProofState | Evolving evidence accumulator |
| SessionKernel | Single state mutation authority |
| SessionLedger | Hash-chained append-only event log |
| TurnKernel | Pure Mealy FSM for turn phases |
| TurnSnapshot | Materialized view of current turn |
| TypedTool | Next-gen tool with const capabilities/purity |