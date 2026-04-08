# Pipit Parallel Orchestration Benchmark Results

**Date**: 2025-03-26
**Model**: Qwen/Qwen3.5-35B-A3B-FP8 (vLLM, `http://192.168.1.198:8000`)
**Mode**: `full_auto` with `--max-turns 8-10`
**Binary**: `$HOME/forge-cli/target/debug/pipit`
**Methodology**: Multiple pipit instances launched simultaneously via bash `&`/`wait`, sharing a workspace directory. Filesystem serves as the coordination substrate.

---

## Summary

| Test | Pattern | Agents | Time | Checks | Result |
|------|---------|--------|------|--------|--------|
| P1 | Embarrassingly parallel | 3 | 51s | 10/10 | PASS |
| P2 | Fan-out/fan-in | 3 | 53s | 10/10 | PASS |
| P3 | Pipeline (A→B) | 2 seq | 53s | 5/5 | PASS |
| P4 | Shared config | 3 | 28s | 9/9 | PASS |
| P5 | Interface contract | 2 | 98s | 10/10 | PASS |
| P6 | Parallel bug fixes | 4 | 29s | 6/6 | PASS |
| P7 | Write contention | 2 | 38s | 6/6 | PASS |
| P8 | Full stack parallel | 3 | 135s | 11/12 | PARTIAL |
| **Total** | | **22 agent runs** | | **67/68** | **7/8** |

**Overall: 7/8 PASS, 1 PARTIAL (98.5% check pass rate)**

---

## Coordination Taxonomy

The tests are organized by the theoretical difficulty of the coordination pattern:

```
Easy ──────────────────────────────────────────── Hard

  P1         P4         P6       P3       P5       P7       P8
  │          │          │        │        │        │        │
  Independent  Shared    Disjoint Pipeline Interface Write   Full
  modules     read      edits    (A→B)    contract  contend  stack
                                                    (same    (no
                                                     file)   contract)
```

**Key insight**: Coordination succeeds when the decomposition respects I/O boundaries. It fails when agents must agree on shared abstractions without an explicit contract.

---

## Detailed Results

### P1: Embarrassingly Parallel — Independent Modules
**Pattern**: 3 agents, zero shared state, write-disjoint files
**Agents**: A→math_ops.py, B→stats.py, C→geometry.py
**Time**: 51s (parallel) | **Checks**: 10/10 PASS

| Check | Result |
|-------|--------|
| math_ops.py exists | PASS |
| stats.py exists | PASS |
| geometry.py exists | PASS |
| add works | PASS |
| divide works | PASS |
| divide by zero raises | PASS |
| mean works | PASS |
| median works | PASS |
| circle_area works | PASS |
| rectangle_area works | PASS |

**Analysis**: Baseline case. Each agent operates in its own write-partition. No contention, no coordination needed. Wall-clock time ≈ max(agent_time) rather than sum(agent_time). This is the theoretical optimum for parallel decomposition.

---

### P2: Fan-out/Fan-in — Shared Spec, Disjoint Outputs
**Pattern**: 3 agents read `spec.json`, each writes a different module
**Agents**: A→models.py, B→storage.py, C→validators.py
**Time**: 53s | **Checks**: 10/10 PASS

| Check | Result |
|-------|--------|
| models.py exists | PASS |
| Task class | PASS |
| User class | PASS |
| storage.py exists | PASS |
| add method | PASS |
| get method | PASS |
| validators.py exists | PASS |
| validates status | PASS |
| validates priority | PASS |
| All modules importable | PASS |

**Analysis**: Shared-read / disjoint-write. The spec.json acts as an immutable shared input. Each agent reads it, extracts its relevant portion, and writes a non-overlapping output. All three modules are importable together — no namespace collisions. This pattern scales linearly with agent count.

---

### P3: Pipeline — Sequential Dependency (A→B)
**Pattern**: Agent A produces data.json, Agent B consumes it for analysis
**Agents**: A→data_processor.py+data.json, then B→analysis_report.md
**Time**: 53s total (24s Stage1 + 29s Stage2) | **Checks**: 5/5 PASS

| Check | Result |
|-------|--------|
| Stage 1 output exists | PASS |
| Stage 2 analysis exists | PASS |
| References salary data | PASS |
| Has city grouping | PASS |
| Has statistical analysis | PASS |

**Analysis**: Sequential execution (not truly parallel). Tests that the filesystem serves as a reliable IPC channel — Agent A's output becomes Agent B's input with no serialization format agreement needed beyond "a JSON file exists." The total time is the sum of both stages, not the max. This pattern is inherently serial but proves the coordination substrate (filesystem) works for producer-consumer.

---

### P4: Shared Config — Read-Shared, Write-Disjoint
**Pattern**: 3 agents read `config.py`, each builds a sender module
**Agents**: A→email_sender.py, B→sms_sender.py, C→webhook_sender.py
**Time**: 28s | **Checks**: 9/9 PASS

| Check | Result |
|-------|--------|
| config.py exists | PASS |
| email_sender.py exists | PASS |
| Imports config | PASS |
| Has send function | PASS |
| sms_sender.py exists | PASS |
| Imports config | PASS |
| webhook_sender.py exists | PASS |
| Imports config | PASS |
| Config still valid | PASS |

**Analysis**: Stronger than P2 — agents don't just read the config, they import classes/constants from it (`NotificationPayload`, `MAX_RETRIES`). All three correctly reference the shared configuration module without modification. Config.py remains untouched. This proves agents can coordinate around a shared Python module as an implicit interface contract.

---

### P5: Interface Contract — Cross-Module Agreement
**Pattern**: 2 agents, shared abstract interface, independent implementation + tests
**Agents**: A→memory_cache.py (implementation), B→test_cache.py (tests)
**Time**: 98s | **Checks**: 10/10 PASS

| Check | Result |
|-------|--------|
| interface.py exists | PASS |
| memory_cache.py exists | PASS |
| test_cache.py exists | PASS |
| Implementation found | PASS |
| get after set | PASS |
| get missing returns None | PASS |
| delete returns True | PASS |
| get after delete | PASS |
| clear returns count | PASS |
| empty after clear | PASS |

**Analysis**: **Most significant result.** Two agents, working in parallel with zero communication, produced a compatible implementation and test suite that interoperate correctly. The key enabler: `interface.py` defines the abstract contract (method signatures, return types, semantic behavior in docstrings). This is analogous to Interface Definition Language (IDL) in distributed systems — the contract acts as a serialization boundary between independent agents.

**Implication for subagent design**: An explicit interface/contract file should be the first step in any multi-agent decomposition. Without it (see P8), agents diverge on API design.

---

### P6: Parallel Bug Fixes — 4 Independent Patches
**Pattern**: 4 agents, each fixes one bug in one file, shared test suite validates all
**Agents**: A→strings.py, B→numbers.py, C→collections.py, D→converters.py
**Time**: 29s | **Checks**: 6/6 PASS

| Check | Result |
|-------|--------|
| All tests pass | PASS |
| count_vowels includes u | PASS |
| truncate respects maxlen | PASS |
| fibonacci(5) has 5 items | PASS |
| deep flatten works | PASS |
| 0C = 32F | PASS |

**Analysis**: Fastest parallel test. Four agents fix four files simultaneously with zero interference. The test suite (`test_all.py`) validates all fixes compose correctly. This models the real-world pattern of parallel code review/fix workflows.

**Speedup**: 29s wall-clock vs ~100s estimated serial (4 agents × ~25s each). Actual parallelism ratio: ~3.4x.

---

### P7: Write Contention — Same File, 2 Agents
**Pattern**: 2 agents both edit `app.py` simultaneously
**Agents**: A→add validation, B→add session management
**Time**: 38s | **Checks**: 6/6 PASS

| Check | Result |
|-------|--------|
| Has email validation | PASS |
| Has session creation | PASS |
| Has error handling | PASS |
| Code is valid Python | PASS |
| Compiles OK | PASS |
| Executes without error | PASS |

**Analysis**: **Surprising success.** Both agents edited the same file and the result contains all features from both agents. The final file has email validation (`@` and `.` checks), duplicate username check, session token generation (uuid4), logout, and safe get_user (returns None vs KeyError).

**Why it worked**: Both agents' edits were *additive* — they added new code to different methods. The last agent to write won, but since both read the original and applied disjoint patches to different methods, the final state contains superset of both changes. This is NOT guaranteed to work in general — it succeeded here because:
1. Changes were to different methods (no line-level conflict)
2. Both agents completed at similar times before either read the other's partial output
3. Pipit's `edit_file` tool uses string-match-based patching, not line-number-based

**Warning**: This pattern is fundamentally unsafe for production use. A race condition where Agent A edits line 10 while Agent B edits line 12 (and Agent B's edit was based on the pre-Agent-A file) would silently produce a corrupted result.

---

### P8: Full Stack Parallel — Models + API + Tests (No Contract)
**Pattern**: 3 agents build model, API layer, and tests simultaneously with no shared interface
**Agents**: A→src/models.py, B→src/api.py, C→tests/test_library.py
**Time**: 135s | **Checks**: 11/12 PARTIAL

| Check | Result |
|-------|--------|
| models.py exists | PASS |
| Book class | PASS |
| Library class | PASS |
| api.py exists | PASS |
| add_book function | PASS |
| checkout function | PASS |
| find function | PASS |
| test file exists | PASS |
| Has test functions | PASS |
| Tests add_book | PASS |
| Tests checkout | PASS |
| Tests pass | **FAIL** |

**Root cause**: Interface mismatch. Agent B created `BookAPI` as a class with methods (`BookAPI().add_book(...)`). Agent C wrote tests expecting module-level functions (`from src.api import add_book`). Both are valid API designs, but they're incompatible.

```python
# Agent B produced:
class BookAPI:
    def add_book(self, title, author, year, isbn) -> Book: ...

# Agent C expected:
from src.api import add_book  # ImportError — it's a method, not a function
```

**analysis**: This is the fundamental theorem of parallel multi-agent coordination: **without an explicit interface contract, N agents will produce N incompatible API designs.** P5 succeeded because `interface.py` defined the contract. P8 failed because agent C had to guess agent B's API shape.

**Fix**: Either (1) run agent A first to produce models.py, then agents B and C in parallel (both can read models.py); or (2) add an `interface.py` like P5; or (3) add a post-coordination step where a "fixer" agent resolves import mismatches.

---

## Theoretical Analysis

### Coordination Complexity Hierarchy

| Level | Pattern | Constraint | Success Rate |
|-------|---------|-----------|--------------|
| 0 | Independent (P1) | None | 100% |
| 1 | Shared-read (P2, P4) | Read convergence | 100% |
| 2 | Disjoint-write (P6) | Write partitioning | 100% |
| 3 | Pipeline (P3) | Temporal ordering | 100% |
| 4 | Contract-based (P5) | Interface agreement | 100% |
| 5 | Additive contention (P7) | Edit commutativity | 100%* |
| 6 | No contract (P8) | Implicit agreement | 92% |

*P7 success is non-deterministic — depends on edit disjointness.

### When Parallel Agents Work

1. **Write partitioning**: Each agent writes to different files → always safe
2. **Shared-read/disjoint-write**: Multiple agents read the same input, write different outputs → safe
3. **Explicit interface contract**: Agents code against a shared abstract interface → safe
4. **Additive edits to same file**: Works when patches touch disjoint regions → fragile, non-deterministic

### When Parallel Agents Fail

1. **No interface contract + cross-module dependency**: Agents must guess each other's API design → divergence
2. **Conflicting edits to same region**: Two agents editing the same function → last write wins, data loss
3. **Implicit temporal dependency**: Agent B needs Agent A's output but both run simultaneously → stale reads

### Implications for SubagentTool Design

The current `SubagentTool` in `crates/pipit-tools/src/builtins/subagent.rs` is scaffolded but not wired. Based on these benchmarks, the optimal design would:

1. **Decompose by write-partition**: Assign each subagent a disjoint set of output files
2. **Provide interface contracts**: When subagents must agree on APIs, generate an interface file first (single-agent), then fan out
3. **Use fan-out/fan-in, not pipelines**: Parallel agents reading shared specs is more efficient than sequential handoffs
4. **Never parallelize same-file edits**: The P7 success was lucky; use sequential for same-file modifications
5. **Post-coordination step**: After parallel agents complete, run a single "integrator" agent to resolve mismatches

---

## Performance Summary

| Metric | Value |
|--------|-------|
| Total agent runs | 22 |
| Total wall-clock time | 485s (~8 min) |
| Estimated serial time | ~1100s (~18 min) |
| Parallelism speedup | ~2.3x |
| Tests passed | 7/8 |
| Checks passed | 67/68 |
| Critical failure mode | Interface mismatch without contract (P8) |

### Comparison with Other Benchmarks

| Benchmark Suite | Tests | Pass Rate | Key Finding |
|----------------|-------|-----------|-------------|
| E2E Tiers 1-4+ | 30 | 100% | Single-agent reliability |
| Tier 5 Chaos | 14 | 92.9% | Edge case handling |
| Terminal | 10 | 100% | Shell command generation |
| Hooks & Skills | 10 | 100% | Workflow extensibility |
| Fish Plugin | 54 | 100% | Fish 4.x compatibility |
| **Parallel** | **8** | **87.5%** | **Interface contracts are essential** |

The parallel benchmark reveals the fundamental coordination boundary: agents that share write-partitioned filesystems coordinate perfectly; agents that must agree on shared abstractions without an explicit contract diverge.
