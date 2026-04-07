# Pipit CLI — End-to-End Benchmark Results

**Model:** Qwen/Qwen3.5-35B-A3B-FP8 (vLLM, FP8 quantization)  
**Endpoint:** http://192.168.1.198:8000 (local inference)  
**Agent Mode:** Non-interactive (`--json --approval full_auto`)  
**Max Turns:** 15 per task  
**Timeout:** 180s per task  
**Date:** 2026-04-06  
**Suite:** 110 real-world test cases (medium → complex)  
**Test Environment:** `/tmp/pipit-e2e-tests` — isolated project with intentional bugs  
**Runner:** `/tmp/pipit-e2e-tests/run_e2e_tests.py`

---

## Executive Summary

| Metric | Value |
|--------|-------|
| **Total Tests** | 110 |
| **Passed** | 55 (50.0%) |
| **Failed** | 55 (50.0%) |
| **Timeouts** | 0 |
| **Total Runtime** | 88.5 minutes |
| **Avg Time/Test** | 48.3s |
| **Fastest Test** | 5s (Test 103 — shell/medium) |
| **Slowest Test** | 201s (Test 001 — analysis/medium) |

---

## Results by Category

| Category | Passed | Total | Rate | Notes |
|----------|--------|-------|------|-------|
| **Analysis** | 13 | 16 | **81%** | Strong at reading + reasoning |
| **Shell** | 4 | 4 | **100%** | Simple bash commands work perfectly |
| **Bug Fix** | 9 | 15 | **60%** | Decent for targeted fixes |
| **Feature** | 13 | 22 | **59%** | Good at file creation, weaker at in-file edits |
| **Docs** | 6 | 11 | **55%** | Can create files, struggles with multi-section docs |
| **Testing** | 5 | 16 | **31%** | Low — often reads but doesn't write test files |
| **Complex** | 2 | 10 | **20%** | Multi-step tasks mostly fail |
| **Refactor** | 3 | 16 | **19%** | Worst category — edits fail silently |

## Results by Difficulty

| Difficulty | Passed | Total | Rate |
|------------|--------|-------|------|
| **Medium** | 32 | 60 | **53%** |
| **Hard** | 16 | 31 | **52%** |
| **Complex** | 7 | 19 | **37%** |

---

## Full Test Results

### Category 1: Code Analysis & Understanding (13/16 — 81%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 001 | medium | Read src/main.py and list all bugs | ✅ PASS | 201s | 54K chars output |
| 002 | medium | Analyze server.py for security vulns | ❌ FAIL | 32s | Model struggled with file paths, never analyzed |
| 003 | medium | Review utils.py for code quality | ✅ PASS | 34s | 26K chars output |
| 004 | medium | Explain calculator.rs parsing algorithm | ✅ PASS | 25s | 25K chars output |
| 005 | medium | Analyze linked_list.c for memory leaks | ✅ PASS | 27s | Found memory + leak keywords |
| 006 | medium | Compare bubble_sort vs quick_sort | ✅ PASS | 24s | 24K chars output |
| 007 | hard | Full code quality report for all src/ | ✅ PASS | 38s | 28K comprehensive analysis |
| 008 | medium | Design patterns in main.py | ✅ PASS | 36s | 44K chars output |
| 009 | medium | Performance bottlenecks in data_pipeline.py | ❌ FAIL | 26s | Didn't use O() notation |
| 010 | hard | Prioritize critical issues across all files | ✅ PASS | 84s | 81K thorough multi-file analysis |
| 011 | medium | Missing test coverage for main.py | ✅ PASS | 37s | 28K chars |
| 012 | medium | Security issues in deploy.sh | ❌ FAIL | 25s | Didn't mention "quote" |
| 013 | hard | Map security exploit data flow paths | ✅ PASS | 29s | 27K chars |
| 014 | medium | Analyze sample.csv dataset | ✅ PASS | 24s | Statistical insights |
| 015 | medium | Compare API docs vs implementation | ✅ PASS | 33s | 43K chars |
| 105 | hard | Write Python script to analyze CSV data | ✅ PASS | 42s | Created 1517-byte script |

### Category 2: Bug Fixing (9/15 — 60%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 016 | medium | Fix division by zero in stats() | ❌ FAIL | 25s | Exhausted 15 turns reading, never edited |
| 017 | medium | Fix case-sensitive search | ❌ FAIL | 25s | File not modified |
| 018 | hard | Fix SQL injection vulnerabilities | ✅ PASS | 24s | Added parameterized queries |
| 019 | medium | Fix memory leak in list_pop() | ✅ PASS | 25s | Added free() call |
| 020 | medium | Fix list_destroy() to free nodes | ✅ PASS | 25s | Proper cleanup |
| 021 | medium | Fix list_peek() null check | ✅ PASS | 30s | Added NULL guard |
| 022 | hard | Replace MD5 with proper password hashing | ❌ FAIL | 34s | Didn't complete the change |
| 023 | medium | Fix CSV export escaping | ✅ PASS | 26s | Used csv module |
| 024 | medium | Fix email validation regex | ✅ PASS | 25s | Improved regex |
| 025 | hard | Fix command injection in run_command() | ✅ PASS | 72s | Added subprocess.run |
| 026 | medium | Fix list_insert() bounds check | ✅ PASS | 25s | Added validation |
| 027 | medium | Fix quick_sort pivot selection | ❌ FAIL | 25s | File unchanged |
| 028 | medium | Add priority validation to add() | ❌ FAIL | 12s | Didn't complete |
| 029 | hard | Fix deploy.sh quoting/validation | ❌ FAIL | 30s | Didn't add proper quoting |
| 030 | medium | Fix retry decorator with backoff | ✅ PASS | 24s | Added time.sleep |

### Category 3: Feature Implementation (13/22 — 59%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 031 | medium | Add due_date to tasks | ❌ FAIL | 26s | File not modified |
| 032 | medium | Add tags/labels system | ❌ FAIL | 87s | Spent time reading, didn't edit |
| 033 | hard | Add JWT authentication | ✅ PASS | 72s | Added token auth |
| 034 | medium | Create BST implementation | ✅ PASS | 47s | Created 3554-byte file |
| 035 | hard | Create rate limiter | ✅ PASS | 64s | Created 4603-byte file |
| 036 | medium | Add counting/radix sort | ❌ FAIL | 89s | Didn't add to existing file |
| 037 | hard | Create LRU cache with TTL | ✅ PASS | 84s | Created 7591-byte file |
| 038 | medium | Add /health endpoint | ❌ FAIL | 24s | Didn't modify server.py |
| 039 | complex | Create Graph with Dijkstra, BFS, DFS | ✅ PASS | 33s | All algorithms present |
| 040 | medium | Add --verbose flag to CLI | ❌ FAIL | 25s | File not modified |
| 041 | hard | Create finite state machine | ✅ PASS | 109s | Created 7523-byte file |
| 042 | medium | Add pagination to /users | ✅ PASS | 68s | Added page/limit params |
| 043 | complex | Create recursive descent parser | ✅ PASS | 146s | Created 6253-byte file |
| 044 | medium | Add archive/unarchive feature | ❌ FAIL | 44s | File not modified |
| 045 | hard | Create connection pool | ✅ PASS | 98s | Created 13678-byte file |
| 046 | medium | Add structured logging | ❌ FAIL | 28s | Didn't add logging import |
| 047 | hard | Create event bus / pub-sub | ✅ PASS | 143s | Created 6872-byte file |
| 048 | medium | Add import/export to TaskManager | ✅ PASS | 24s | Added import/export methods |
| 049 | complex | Create Bloom filter | ✅ PASS | 79s | Created 5669-byte file |
| 050 | hard | Create task scheduler | ❌ FAIL | 62s | Model thought but made zero tool calls |
| 106 | hard | Create middleware chain | ❌ FAIL | 62s | File not created |
| 107 | complex | Create pub/sub system | ✅ PASS | 109s | Created 11424-byte file |

### Category 4: Testing (5/16 — 31%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 051 | medium | Comprehensive unit tests for TaskManager | ✅ PASS | 26s | Added test methods |
| 052 | medium | Create test_utils.py | ✅ PASS | 68s | Created 7916-byte file |
| 053 | hard | Create test_server.py integration tests | ❌ FAIL | 21s | File not created |
| 054 | medium | Property-based sort tests | ❌ FAIL | 82s | test_sort.py not created |
| 055 | hard | Create test_pipeline.py | ❌ FAIL | 23s | File not created |
| 056 | medium | Edge case tests (unicode, etc) | ❌ FAIL | 44s | Didn't add unicode tests |
| 057 | hard | Test C linked list via subprocess | ❌ FAIL | 26s | File not created |
| 058 | medium | Binary search tests | ❌ FAIL | 42s | No test file created |
| 059 | complex | Test fixture generator conftest.py | ❌ FAIL | 29s | File not created |
| 060 | medium | Test Rust calculator via subprocess | ✅ PASS | 69s | Created 6709-byte file |
| 061 | hard | Load testing script | ✅ PASS | 76s | Created 9644-byte file |
| 062 | medium | Mutation testing comments | ❌ FAIL | 27s | File unchanged |
| 063 | hard | Security-focused tests | ❌ FAIL | 74s | File not created |
| 064 | medium | Coverage analysis script | ✅ PASS | 26s | Created 289-byte script |
| 065 | complex | Full integration test workflow | ❌ FAIL | 34s | File not created |
| 108 | hard | Performance benchmarks for sorting | ❌ FAIL | 27s | File not created |

### Category 5: Refactoring (3/16 — 19%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 066 | medium | Refactor to dataclasses | ❌ FAIL | 24s | No @dataclass added |
| 067 | hard | Refactor to router pattern | ❌ FAIL | 25s | server.py unchanged |
| 068 | medium | Refactor to generators | ❌ FAIL | 72s | No yield added |
| 069 | hard | Split utils.py into modules | ❌ FAIL | 24s | New files not created |
| 070 | medium | Sort algorithm common interface | ❌ FAIL | 25s | File unchanged |
| 071 | hard | Separate server into db.py/handlers.py | ✅ PASS | 50s | Created 1690-byte db.py |
| 072 | medium | Fix flatten() for deep nesting | ✅ PASS | 24s | Added recursive flatten |
| 073 | complex | Repository pattern for TaskManager | ❌ FAIL | 26s | No Repository class |
| 074 | medium | Create linked_list.h header | ❌ FAIL | 26s | File not created |
| 075 | hard | Add lazy evaluation to DataPipeline | ❌ FAIL | 70s | No lazy mode |
| 076 | medium | Add type hints to main.py | ❌ FAIL | 24s | Missing type annotations |
| 077 | hard | Refactor to async I/O | ❌ FAIL | 138s | No async/asyncio added |
| 078 | medium | Add calculator history | ❌ FAIL | 28s | No history added |
| 079 | complex | Python package structure | ✅ PASS | 75s | Created __init__.py (1268 bytes) |
| 080 | medium | Simplify merge_sort | ❌ FAIL | 32s | File unchanged |
| 109 | complex | Clean architecture refactor | ❌ FAIL | 28s | Directory not created |

### Category 6: Documentation (6/11 — 55%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 081 | medium | Comprehensive README.md | ❌ FAIL | 55s | Missing install/usage sections |
| 082 | medium | Google-style docstrings for pipeline | ✅ PASS | 172s | Added Args:/Returns: |
| 083 | hard | Create ARCHITECTURE.md | ❌ FAIL | 38s | File not created |
| 084 | medium | Update API.md with examples | ❌ FAIL | 28s | Missing response examples |
| 085 | hard | Create SECURITY.md | ❌ FAIL | 47s | File not created |
| 086 | medium | Create CHANGELOG.md | ✅ PASS | 12s | Created 598-byte file |
| 087 | medium | Create ALGORITHMS.md | ❌ FAIL | 8s | File not created |
| 088 | hard | Create CONTRIBUTING.md | ✅ PASS | 33s | Created 5964-byte file |
| 089 | medium | Create Makefile | ✅ PASS | 52s | Created 2386-byte file |
| 090 | medium | Create CI workflow YAML | ✅ PASS | 24s | Created 3196-byte file |
| 110 | medium | Create .editorconfig/.gitignore/.pre-commit | ✅ PASS | 67s | Created config files |

### Category 7: Complex Multi-Step Tasks (2/10 — 20%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 091 | complex | REST API client library | ❌ FAIL | 62s | File not created |
| 092 | complex | CLI argument parser from scratch | ❌ FAIL | 66s | File not created |
| 093 | complex | Minimal ORM | ✅ PASS | 99s | Created 8953-byte file |
| 094 | complex | Template engine | ❌ FAIL | 67s | File not created |
| 095 | complex | Regex engine from scratch | ✅ PASS | 84s | Created 9511-byte file |
| 096 | complex | Trie-based HTTP router | ❌ FAIL | 62s | File not created |
| 097 | complex | Full security audit + fixes | ❌ FAIL | 68s | Audit file not created |
| 098 | complex | JSON parser from scratch | ❌ FAIL | 65s | File not created |
| 099 | complex | Dockerfile + docker-compose | ❌ FAIL | 35s | Neither file created |
| 100 | complex | Async worker pool | ❌ FAIL | 62s | File not created |

### Category 8: Shell & Misc (4/4 — 100%)

| # | Difficulty | Prompt | Status | Time | Detail |
|---|-----------|--------|--------|------|--------|
| 101 | medium | List Python files + line counts | ✅ PASS | 7s | 7K chars output |
| 102 | medium | Find TODO/FIXME comments | ✅ PASS | 6s | 9K chars output |
| 103 | medium | Find files importing os module | ✅ PASS | 5s | 5K chars output |
| 104 | hard | Create linting script | ✅ PASS | 36s | Created 2068-byte script |

---

## Failure Analysis

### Root Cause Distribution

| Root Cause | Count | % of Failures |
|------------|-------|---------------|
| **File not created/modified** | 33 | 60% |
| **Validator pattern mismatch** | 12 | 22% |
| **Max turns exhausted** (15 turns reading, no edits) | 7 | 13% |
| **Zero tool calls** (model thinks but doesn't act) | 3 | 5% |

### Root Cause Details

#### 1. File Not Created/Modified (60% of failures)
The most common failure mode. The model either:
- Spent all turns reading existing files but never invoked write_file/edit_file
- Produced a thinking/content response about what it would do, but the turn ended without a tool call
- Failed to navigate to the correct file path within the sandboxed workdir

**Affected categories:** Refactor (13 of 16 failures), Testing (11 of 16), Complex (8 of 10)

#### 2. Validator Pattern Mismatch (22% of failures)  
The model made edits but didn't include the expected keywords. Examples:
- Test 009: Model discussed performance but didn't use "O(" notation
- Test 012: Analyzed deploy.sh security but didn't use the word "quote"
- Test 016: Fixed the bug differently than expected (no "0" in the fix)

#### 3. Max Turns Exhausted (13% of failures)
The model burned through all 15 turns reading files, exploring the directory, and thinking, but never committed an edit. This is especially common for bug fix tasks where the model repeatedly tries `cat` and `grep` instead of making targeted edits.

#### 4. Zero Tool Calls (5% of failures)
The model generated thinking/reasoning text but produced no tool calls at all. The response ended after the `</think>` tag without any function invocation. This appears to be a Qwen3.5 tool-use reliability issue.

---

## Performance Characteristics

### Response Time Distribution

| Bucket | Count | % |
|--------|-------|---|
| 0-10s | 5 | 5% |
| 10-30s | 44 | 40% |
| 30-60s | 24 | 22% |
| 60-120s | 28 | 25% |
| 120-180s | 8 | 7% |
| >180s | 1 | 1% |

**Median:** ~35s  
**P90:** ~100s  
**P99:** ~172s  

### Token Usage
Average: ~700-1200 tokens per task (varies by turns used)

### Strengths
1. **Code analysis** (81%) — The model excels at reading code, identifying patterns, and explaining them
2. **Shell commands** (100%) — Simple bash operations work flawlessly
3. **New file creation** (when attempted) — Creates well-structured, substantial files (3-13KB)
4. **Security analysis** — Found SQL injection, command injection, memory leaks

### Weaknesses
1. **In-file editing** — The model often reads but fails to edit existing files
2. **Refactoring** (19%) — Almost never successfully modifies existing code structure
3. **Multi-step complex tasks** (20%) — Can't maintain focus across many sequential operations
4. **Tool use reliability** — Some turns produce zero tool calls despite clear thinking
5. **Path resolution** — Occasional confusion about relative vs absolute file paths in the workdir

---

## Recommendations

### For Pipit Agent Improvements
1. **Auto-retry on zero tool calls** — Detect when the model produces content but no tool calls, and re-prompt with tool use instructions
2. **Smarter turn budgeting** — Reserve at least 5 turns for editing after reading; warn when turns are running low
3. **File path hints** — Include the absolute workdir path in the system prompt so the model doesn't waste turns on path discovery
4. **Edit fallback** — If edit_file fails, automatically try write_file with the full modified content

### For Model Selection
1. **Qwen3.5-35B-A3B-FP8** is strong for analysis tasks but unreliable for tool-heavy workflows
2. Consider larger models (70B+) for complex multi-file tasks
3. The model's `</think>` reasoning mode may interfere with tool calling — consider disabling extended thinking for action-heavy prompts

### For Test Suite
1. Relax pattern matching — use looser validators for "did the model attempt the task" rather than exact keyword matching
2. Increase max_turns to 25-30 for complex/refactor tasks
3. Add validators that check git diff for any modification, not just specific patterns

---

## Test Project Description

The test suite runs against a synthetic project at `/tmp/pipit-e2e-tests` containing:

| File | Purpose | Size |
|------|---------|------|
| `src/main.py` | Task manager with 6 intentional bugs | 2.3KB |
| `src/server.py` | HTTP API with SQL injection, weak auth | 2.1KB |
| `src/utils.py` | Utility functions with code smells | 2.5KB |
| `src/calculator.rs` | Rust expression parser with bugs | 2.4KB |
| `src/linked_list.c` | C linked list with memory leaks | 2.8KB |
| `src/sort_algorithms.py` | Sorting algorithms with perf issues | 2.7KB |
| `src/data_pipeline.py` | Data pipeline with O(n*m) joins | 2.3KB |
| `tests/test_main.py` | Incomplete test file | 0.5KB |
| `docs/API.md` | Incomplete API documentation | 0.4KB |
| `scripts/deploy.sh` | Deployment script with issues | 0.3KB |
| `data/sample.csv` | 10-row employee dataset | 0.4KB |

Each test case gets a fresh copy of this project (via `shutil.copytree`), initialized as a git repo, to ensure test isolation.

---

## Reproduction

```bash
# Build pipit
cd /Users/sushanth/forge-cli && cargo build -p pipit-cli

# Run full suite
python3 /tmp/pipit-e2e-tests/run_e2e_tests.py --start 1 --end 110 --timeout 180

# Run single test
python3 /tmp/pipit-e2e-tests/run_e2e_tests.py --test 42

# Results
cat /tmp/pipit-e2e-results/results.json | python3 -m json.tool
ls /tmp/pipit-e2e-results/test_*.log
```

---

*Generated by Pipit E2E Test Suite v1.0 — 110 test cases, 88.5 minutes runtime*
