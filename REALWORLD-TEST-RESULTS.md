# Pipit CLI — Real-World Scenario Test Results

**Date**: 2026-03-29  
**Model**: Qwen/Qwen3.5-35B-A3B-FP8 (local vLLM @ http://192.168.1.198:8000)  
**Binary**: pipit v0.1.6 (debug build)  
**Mode**: full_auto, single-shot (non-interactive)

---

## Summary

| # | Scenario | Result | Turns | Tests Written | Tests Passed | Verdict |
|---|----------|--------|-------|---------------|--------------|---------|
| 1 | Fix bugs in Flask order API | PASS | 6 | 0 (existing) | 5/5 | All bugs fixed, minimal diff |
| 2 | Add pagination to catalog module | PASS | 10 | 15 | 15/15 | Feature + edge case tests |
| 3 | Security refactor (MD5→SHA256, etc.) | PASS | 8 | 0 (existing) | 6/6 | All 4 security fixes applied |
| 4 | Debug CLI + add move command | PASS | 15 | 16 | 16/16 | Multi-file fix + new feature |
| 5 | Create deployment script | PASS | 9 | N/A | N/A | Professional quality script |
| 6 | Build data pipeline from raw data | PASS | 5 | N/A | N/A | Correct calculations verified |

**Overall: 6/6 PASS (100%)**

---

## Test 1: Fix Bugs in Flask Order API

**Scenario**: A Flask app has an orders API with 3 bugs: no status validation on cancel/ship, cancelled orders counted in revenue, by_status not implemented. 5 failing tests provided.

**Prompt**: "The tests are failing. Fix all bugs in app.py so every test passes. Run pytest to verify."

**Result**: PASS in 6 turns
- Read test file and app.py
- Ran pytest to see failures
- Applied 3 targeted edits (status check on cancel, status check on ship, fix summary)
- Ran pytest → all 5 pass

**Diff quality**: Minimal — only 3 surgical edits, no unnecessary changes. Didn't touch `create_order` or `list_orders` (which were correct).

---

## Test 2: Add Pagination Feature

**Scenario**: A product catalog module with `search_products()` that returns all results. Need pagination with `page`, `per_page`, and proper response format.

**Prompt**: "Add pagination to the search_products function... Write tests... Run the tests."

**Result**: PASS in 10 turns
- Modified `search_products` to accept page/per_page params
- Wrote `test_catalog.py` with **15 tests** including:
  - Default pagination, custom page/per_page
  - Page 0, negative page, page beyond total
  - Exact division, remainder handling
  - Filters combined with pagination
  - Per-page larger than total, per-page zero

**Edit quality**: Clean. Return format: `{items, page, per_page, total, total_pages}`. Existing code preserved.

---

## Test 3: Security Refactor

**Scenario**: Auth module using MD5 hashing, `random` for tokens, no session expiry, timing-vulnerable comparison. 6 existing tests must keep passing.

**Prompt**: "Refactor auth.py to fix the security issues: SHA-256, secrets module, session expiry, hmac.compare_digest."

**Result**: PASS in 8 turns
- All 4 security improvements applied:
  - `hashlib.md5()` → `hashlib.sha256()` 
  - `random.choices()` → `secrets.token_urlsafe()`
  - Added `time.time() - s["created"] > 3600` session expiry check
  - `==` → `hmac.compare_digest()` for password comparison
- All 6 existing tests still pass
- No behavioral regression — same API, better security

---

## Test 4: Debug Failing CLI + Add Feature

**Scenario**: A task management CLI where the `done` command crashes because it tries to skip status transitions (todo → done, but the model requires todo → in_progress → review → done). Need to fix the bug AND add a `move` command.

**Prompt**: "Users are reporting that 'done' fails. Fix it to properly transition through statuses. Add a 'move' command. Write tests."

**Result**: PASS in 15 turns (most complex task)
- Fixed `cmd_done` to auto-transition through all required steps
- Added `cmd_move` for explicit status changes
- Wrote 16 tests covering:
  - `done` from various states
  - `move` for all valid transitions
  - Invalid transitions, missing args, nonexistent tasks
- End-to-end CLI workflow verified: `add → list → move → done → list`

---

## Test 5: Create Deployment Script

**Scenario**: Need a professional deployment script from scratch.

**Prompt**: "Create deploy.sh with environment arg, dry-run, error handling, colored output, no hardcoded secrets."

**Result**: PASS in 9 turns
- Created 248-line `deploy.sh` with:
  - `set -e` for error handling
  - Colored output (INFO/WARNING/ERROR/SUCCESS)
  - `--dry-run` flag showing what would happen
  - Environment validation (staging/production only)
  - Proper usage text and exit codes
  - Simulated test/build/deploy steps
- `deploy.sh --dry-run staging` produces clean formatted output
- `deploy.sh` (no args) shows usage
- `deploy.sh invalid_env` shows error

---

## Test 6: Build Data Pipeline

**Scenario**: JSON orders + TSV inventory. Build a pipeline that filters, aggregates, joins, and produces a report.

**Prompt**: "Build pipeline.py that reads orders.json + inventory.tsv, calculates revenue, finds low-stock items, top customer. Write report.json."

**Result**: PASS in 5 turns (fastest task)
- Correct calculations verified manually:
  - Total revenue: $819.86 (excluded cancelled + pending orders) ✓
  - Revenue per SKU: SKU-A $209.93, SKU-B $199.96, SKU-C $399.98, SKU-D $9.99 ✓
  - Low stock alerts: SKU-B (stock 3 < reorder 5), SKU-C (stock 0 < reorder 2) ✓
  - Top customer: Dave $399.98 ✓

---

## Developer Experience Notes

### What feels natural:
- Turn-by-turn progress shows what pipit is thinking
- Tool call results show clear success/failure icons
- Agent explains its reasoning before each action
- Tests are run automatically to verify fixes
- Summary at the end explains what was changed and why

### What could be improved:
- The "Proof packet" section at the end uses jargon ("CharacterizationFirst", "Risk score: 0.2027", "Realized edits") that regular developers won't understand
- Strategy names like "MinimalPatch" and "RootCauseRepair" are internal engineering terms
- Truncated diffs in "Realized edits" repeat info the developer already saw in the turn output

### No issues found:
- No crashes on any scenario
- No weird/unexpected behavior
- No hallucinated files or phantom edits
- Tests actually pass (not just claimed to pass)
- Diffs are clean and minimal
- Agent doesn't modify files it shouldn't
- cd stays within project root

---

## How each test was run

```bash
# Setup
cd /tmp/pipit-realworld/testN
# ... create source files, git init, git commit ...

# Run pipit  
/path/to/pipit "prompt describing the task" \
  --provider openai --model Qwen/Qwen3.5-35B-A3B-FP8 \
  --base-url http://192.168.1.198:8000 --api-key dummy \
  --approval full_auto --max-turns 15

# Verify
python3 -m pytest test_*.py -v
# or: bash script.sh --dry-run staging
# or: cat report.json
```

Workspaces preserved at `/tmp/pipit-realworld/test{1..6}` for inspection.
