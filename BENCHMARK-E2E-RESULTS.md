# Pipit CLI Agent — E2E Benchmark Results

**Date**: 2026-03-26
**Model**: `Qwen/Qwen3.5-35B-A3B-FP8` (local, vLLM @ `http://192.168.1.198:8000`)
**Provider**: OpenAI-compatible
**Approval mode**: `full_auto`
**Binary**: `pipit 0.1.0` (debug build)

---

## Scoring Rubric

| Metric                         | Weight | 10/10                                           | 7/10                        | 4/10                           | 0/10                      |
| ------------------------------ | -----: | ----------------------------------------------- | --------------------------- | ------------------------------ | ------------------------- |
| **Correctness**                |    40% | All visible + hidden checks pass                | Main issue fixed, minor miss | Partial fix, regressions       | Wrong fix or broken repo  |
| **Edit Quality**               |    20% | Minimal, targeted, style-consistent diff        | Slight over-editing          | Broad churn or unnecessary     | Damaging or chaotic edits |
| **Reasoning / Diagnosis**      |    15% | Clear diagnosis, right abstraction level         | Mostly right but shallow     | Guessed into working patch     | Misdiagnosed issue        |
| **Verification**               |    15% | Runs tests, adds coverage, checks edge cases    | Runs basic tests only        | Weak verification              | No real verification      |
| **Robustness / Compatibility** |    10% | Preserves contracts, handles malformed inputs   | Minor compat risk            | Noticeable regressions         | Breaks compatibility      |

---

## Tier 1 — Basic Reliability

| #  | Test Case                  | Result | Turns | Correctness | Edit Quality | Reasoning | Verification | Robustness | Notes |
| -- | -------------------------- | ------ | ----: | ----------: | -----------: | --------: | -----------: | ---------: | ----- |
| 1  | File creation from scratch | PASS   |     3 |       10/10 |        10/10 |      9/10 |         9/10 |       10/10 | Created `string_utils.py` with 5 functions, full docstrings, type hints. Self-verified by reading back + running. |
| 2  | Bug fix in existing code   | PASS   |     4 |       10/10 |        10/10 |     10/10 |        10/10 |       10/10 | Found & fixed all 3 bugs in `calculator.py` (divide-by-zero, power impl, modulo check). Ran 6-point verification. |
| 3  | Security refactor          | PASS   |     8 |        9/10 |         7/10 |      8/10 |         6/10 |        8/10 | Added SHA256 hashing, KeyError handling, `hmac.compare_digest`. Applied edits one-by-one (could batch). Did not verify by running. |
| 4  | Multi-file feature addition| PASS   |    12 |        9/10 |         7/10 |      8/10 |         8/10 |        8/10 | Created `logger.py`, integrated into calculator + user_manager across 3 files. Verified with test run. |
| 5  | Test generation            | PASS   |    15 |       10/10 |         7/10 |      7/10 |        10/10 |        9/10 | Wrote 27 pytest tests. Had 4 assertion mismatches (off-by-one on truncation), self-corrected over 4 iterations until green. |
| 6  | Code review / explanation  | PASS   |     3 |        9/10 |        10/10 |      9/10 |         N/A |        N/A | Read all 4 source files, produced structured review with line numbers, severity ratings, and concrete fix suggestions. |

### Tier 1 Observations

- **Planning system**: pipit picked appropriate plans (MinimalPatch, CharacterizationFirst) and pivoted when verification failed
- **Self-correction**: Test 5 showed pipit iterating through 4 rounds of test failures, diagnosing wrong expected values, and fixing them
- **Proof packets**: Every run produced a proof packet with rollback checkpoint, confidence score, and realized edits
- **Tool usage**: used `write_file`, `edit_file`, `bash`, and `read_file` tools effectively
- **Local model**: 35B Qwen model handled all tasks correctly through OpenAI-compatible endpoint

---

## Tier 2 — Engineering Competence

| #  | Test Case                             | Result | Turns | Correctness | Edit Quality | Reasoning | Verification | Robustness | Notes |
| -- | ------------------------------------- | ------ | ----: | ----------: | -----------: | --------: | -----------: | ---------: | ----- |
| 7  | Ambiguous spec resolution             | PASS   |     8 |        9/10 |         8/10 |      9/10 |         8/10 |        8/10 | Implemented notification system from vague spec, stated assumptions clearly. |
| 8  | Large-repo code navigation            | PASS   |    16 |       10/10 |         9/10 |     10/10 |         9/10 |        9/10 | Traced pricing bug through 18-file repo, found & fixed root cause. |
| 9  | Hidden failing tests only             | PASS   |    11 |       10/10 |         9/10 |      9/10 |        10/10 |        9/10 | Found all 4 hidden edge cases (page=0, negative page, per_page=0, missing key). |
| 10 | Flaky test diagnosis                  | PASS   |    11 |       10/10 |         8/10 |      9/10 |         9/10 |        9/10 | Fixed timing deps + shared state pollution causing test flakiness. |
| 11 | Performance regression fix            | PASS   |    13 |       10/10 |         9/10 |     10/10 |         9/10 |        9/10 | Optimized 4 O(n²) functions to O(n), recursive→iterative for deep nesting. |
| 12 | Backward-compatible refactor          | PASS   |    13 |       10/10 |         8/10 |      8/10 |        10/10 |       10/10 | Refactored internals, all 13 API contract tests still pass. |
| 13 | Dependency upgrade breakage           | PASS   |    13 |       10/10 |         9/10 |     10/10 |        10/10 |        9/10 | Fixed all 9 pandas 1.x→2.0 breaking changes, all 5 tests pass. |
| 14 | Security hardening bundle             | PASS   |    14 |       10/10 |         8/10 |      9/10 |         8/10 |        9/10 | Fixed 10 vulnerabilities: weak secret, temp dir, session IDs, logging, path traversal, etc. |

---

## Tier 3 — Adversarial Debugging

| #  | Test Case                             | Result | Turns | Correctness | Edit Quality | Reasoning | Verification | Robustness | Notes |
| -- | ------------------------------------- | ------ | ----: | ----------: | -----------: | --------: | -----------: | ---------: | ----- |
| 15 | Concurrency / race condition          | PASS   |    11 |       10/10 |         8/10 |      9/10 |         9/10 |       10/10 | Added proper locking with deadlock avoidance via consistent lock ordering. |
| 16 | Merge conflict resolution             | PASS   |     7 |        9/10 |         9/10 |      9/10 |         8/10 |        9/10 | Resolved semantic conflicts, preserved both branch intents. |
| 17 | Schema / API contract migration       | PASS   |    12 |       10/10 |         9/10 |     10/10 |        10/10 |       10/10 | Migrated V1→V2 schema (8 changes), all 12 contract tests pass. |
| 18 | Minimal-diff requirement              | PASS   |     3 |       10/10 |        10/10 |     10/10 |         8/10 |       10/10 | Applied minimal 2-line fix, only touched the broken function. |
| 19 | Logging/observability addition        | PASS   |     8 |       10/10 |         9/10 |      9/10 |        10/10 |       10/10 | Added structured logging, card_token NOT logged (security), all 7 tests pass. |
| 20 | CLI or config behavior preservation   | PASS   |    12 |       10/10 |         9/10 |      9/10 |        10/10 |       10/10 | Added `validate` subcommand, all 20 existing tests pass. |

---

## Tier 4 — Full-System Adversarial

| #  | Test Case                             | Result | Turns | Correctness | Edit Quality | Reasoning | Verification | Robustness | Notes |
| -- | ------------------------------------- | ------ | ----: | ----------: | -----------: | --------: | -----------: | ---------: | ----- |
| 21 | Multi-language boundary fix           | PASS   |     4 |       10/10 |        10/10 |     10/10 |         9/10 |       10/10 | Fixed 3 Python+shell boundary bugs: shell=True→list args, $1 quoting. Minimal. |
| 22 | Test repair with bad existing tests   | PASS   |     8 |       10/10 |         9/10 |     10/10 |        10/10 |       10/10 | Fixed 6/12 broken tests, didn't touch inventory.py (correct constraint). |
| 23 | Unicode / locale edge cases           | PASS   |    14 |       10/10 |         9/10 |      9/10 |        10/10 |       10/10 | Fixed all 10 Unicode bugs: unicodedata.normalize for accents, Unicode whitespace, safe truncation. |
| 24 | File-format preservation              | PASS   |     7 |       10/10 |         9/10 |      9/10 |        10/10 |       10/10 | Fixed get_nested, set_nested, validate_config. Format preserved on save. |
| 25 | Rollback-safe feature addition        | PASS   |    10 |       10/10 |         9/10 |      9/10 |         9/10 |       10/10 | Implemented fuzzy search + volume discount behind flags. All 14 old + 3 new tests pass. |
| 26 | Incomplete or misleading docs         | PASS   |     6 |        9/10 |         9/10 |      9/10 |         N/A  |        N/A | Replaced all TODOs with accurate API docs from code. Examples included. |
| 27 | Error-handling audit                  | PASS   |     9 |       10/10 |         9/10 |      9/10 |        10/10 |       10/10 | Added with-statements, ValueError for unsupported format, batch error collection. 17/17 pass. |
| 28 | State leakage across tests            | PASS   |     7 |       10/10 |        10/10 |     10/10 |        10/10 |       10/10 | Created conftest.py with autouse fixture to reset global state. Minimal, elegant. |
| 29 | Precision edit in large file          | PASS   |     5 |       10/10 |        10/10 |     10/10 |        10/10 |       10/10 | Found one-line bug (n→n-1) in 539-line file. 1-line fix. 10/10 pass. |
| 30 | Regression bundle                     | PASS*  |  20+5 |        8/10 |         7/10 |      8/10 |         9/10 |        8/10 | Fixed 5 bugs in cart, but introduced new bug in grand_total (hardcoded tax). Required 2nd run to fix. |

---

## Aggregate Scores

| Tier | Tests | Passed | Failed | Pass Rate |
| ---- | ----: | -----: | -----: | --------: |
| 1    |     6 |      6 |      0 |      100% |
| 2    |     8 |      8 |      0 |      100% |
| 3    |     6 |      6 |      0 |      100% |
| 4    |    10 |     10 |      0 |      100% |
| **Total** | **30** | **30** | **0** | **100%** |

*Test 30 required a second pipit run (PASS*) — first run fixed 3/5 bugs but introduced a new one in `grand_total` (hardcoded 0.08 tax rate). Second run completed the fix in 5 turns.

---

## Tier 2 Observations

- **Ambiguous spec (7)**: pipit stated its assumptions and built a coherent notification system from a vague request
- **Large-repo navigation (8)**: Impressive 18-file traversal to find the root cause; pipit used repo map effectively
- **Hidden edge cases (9)**: Found non-obvious edge cases (page=0, per_page=0, negative page) without hints
- **Flaky tests (10)**: Correctly diagnosed both timing dependency AND shared state pollution
- **Performance (11)**: Identified O(n²) patterns and applied correct optimization (sets, iterative recursion)
- **Backward compat (12)**: Refactored internals while keeping all 13 contract tests green
- **Dependency upgrade (13)**: Identified all 9 pandas 2.0 breaking changes by name — deep API knowledge
- **Security (14)**: Found 10 distinct vulnerabilities across multiple categories

## Tier 3 Observations

- **Concurrency (15)**: Applied consistent lock ordering to avoid deadlocks — sophisticated fix
- **Merge conflict (16)**: Resolved semantic conflicts (not just text), preserving both features
- **Schema migration (17)**: Complete V1→V2 migration touching models + services, all 12 tests pass
- **Minimal diff (18)**: Outstanding discipline — touched only 2 lines in the broken function
- **Logging (19)**: Added structured logging without logging sensitive data (card_token). Security-aware
- **CLI preservation (20)**: Extended CLI cleanly; all 20 existing tests pass with new subcommand

## Tier 4 Observations

- **Multi-language (21)**: Fixed Python AND shell files in same session, understood cross-language boundaries
- **Test repair (22)**: Correctly identified test bugs vs code bugs; fixed only tests, left code alone
- **Unicode (23)**: Used unicodedata.normalize for accent stripping, handled Unicode whitespace classes
- **Format preservation (24)**: Fixed 3 bugs while preserving JSON formatting on write-back
- **Feature flags (25)**: Implemented fuzzy search (edit distance) + volume discount behind flags. Old + new tests pass
- **Docs (26)**: Generated accurate API documentation from code with examples and edge case notes
- **Error handling (27)**: Added proper resource management (with-statements), error collection in batch ops
- **State leakage (28)**: Elegant conftest.py solution — didn't modify any existing files
- **Precision edit (29)**: Found the bug in a 539-line file and applied a 1-character fix (`n` → `n-1`)
- **Regression bundle (30)**: Fixed 5 interrelated bugs, BUT first run introduced a regression — required retry

---

## Key Findings

### Strengths
1. **Planning system**: MinimalPatch and CharacterizationFirst plans selected appropriately per task
2. **Self-correction**: Reliable loop of run tests → diagnose failure → fix → re-run
3. **Constraint compliance**: Respected "do not modify test files" constraints in all tests
4. **Security awareness**: Didn't log sensitive data, used proper shell quoting, found injection vulnerabilities
5. **Cross-language understanding**: Handled Python+shell boundary issues correctly
6. **Proof packets**: Every run produced rollback checkpoints and confidence scores

### Weaknesses
1. **Complex multi-bug fixes (Test 30)**: When fixing 5 bugs simultaneously, introduced a regression (hardcoded tax rate)
2. **Turn efficiency**: Some tasks took more turns than necessary due to `python` vs `python3` binary issue
3. **First-run success rate**: 29/30 passed on first run (96.7%). Test 30 needed a second prompt

### Overall Assessment
- **30/30 tests passed** (29 first-run, 1 required retry)
- Average turns per task: ~9.5
- Model (Qwen 35B-A3B) performed remarkably well on a local vLLM endpoint
- Pipit's planning system, proof packets, and rollback checkpoints worked correctly throughout
