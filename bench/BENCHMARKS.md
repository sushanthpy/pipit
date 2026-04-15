# Benchmarks

Pipit is benchmarked against [SWE-bench](https://swe-bench.github.io/) — a dataset of real GitHub issues from popular Python repositories. The agent is given an issue description and must produce a patch that fixes it.

## Quick Start

### Prerequisites

- pipit binary in PATH (`cargo build --release && cp target/release/pipit ~/.local/bin/`)
- Python 3.10+
- An LLM provider (local or hosted)

### Install benchmark dependencies

```sh
cd mini-swe-agent
python -m venv .venv
source .venv/bin/activate
pip install -e .
pip install datasets
```

### Run benchmarks

```sh
# Smoke test (1 trivial instance)
python run_pipit_bench.py \
  --subset _test --split test \
  --model Qwen/Qwen3.5-35B-A3B-FP8 \
  --provider openai \
  --api-key dummy \
  --base-url http://localhost:8000 \
  -o results/pipit-test

# SWE-bench Lite — first 5 instances
python run_pipit_bench.py \
  --subset lite --split test --slice 0:5 \
  --model Qwen/Qwen3.5-35B-A3B-FP8 \
  --provider openai \
  --api-key dummy \
  --base-url http://localhost:8000 \
  -o results/pipit-lite

# SWE-bench Lite — full (300 instances)
python run_pipit_bench.py \
  --subset lite --split test \
  --model claude-sonnet-4-20250514 \
  --provider anthropic \
  --api-key "$ANTHROPIC_API_KEY" \
  --timeout 900 \
  -o results/pipit-lite-full
```

### CLI options

| Flag | Default | Description |
|---|---|---|
| `--subset` | `_test` | Dataset: `_test`, `lite`, `verified`, `full` |
| `--split` | `test` | Dataset split |
| `--slice` | (all) | Instance range, e.g. `0:5` |
| `--model` | (required) | Model name |
| `--provider` | `openai` | LLM provider |
| `--api-key` | | API key |
| `--base-url` | | Provider base URL override |
| `--timeout` | 600 | Seconds per instance |
| `--force` | | Re-run completed instances |
| `-o` | `results/pipit-bench` | Output directory |

### Resume support

The runner writes `preds.json` incrementally. If interrupted, re-running the same command skips completed instances automatically. Use `--force` to re-run everything.

## How it works

1. **Clone** — the target repo is cloned into a temp directory at the base commit
2. **Run pipit** — pipit is invoked in single-shot mode with the issue as the prompt
3. **Collect patch** — `git diff` extracts pipit's changes
4. **Save** — results are written in SWE-bench evaluation format (`preds.json`)

## Output format

```
results/pipit-lite/
├── preds.json        # SWE-bench predictions (instance_id -> patch)
└── results.jsonl     # Per-instance metadata (timing, status)
```

`preds.json` is directly compatible with [`swebench.harness.run_evaluation`](https://github.com/princeton-nlp/SWE-bench).

## Evaluate results

```sh
pip install swebench
python -m swebench.harness.run_evaluation \
  --predictions_path results/pipit-lite/preds.json \
  --swe_bench_tasks princeton-nlp/SWE-Bench_Lite \
  --log_dir results/pipit-lite/eval_logs \
  --testbed /tmp/swebench_testbed
```

## Early results

| Dataset | Model | Instances | Patches | Notes |
|---|---|---|---|---|
| SWE-bench test (dummy) | Qwen3.5-35B-A3B-FP8 (local) | 1 | 1 (100%) | Smoke test — trivial syntax fix |
| SWE-bench Lite | Qwen3.5-35B-A3B-FP8 (local) | 1 | 1 (100%) | astropy separability matrix bug |

Full benchmark runs across SWE-bench Lite (300 instances) and Verified (500 instances) are in progress.

## Architecture: mini-swe-agent integration

Pipit also integrates with [mini-swe-agent](https://github.com/klieret/mini-swe-agent) for Docker-based evaluation:

```
mini-swe-agent/
├── src/minisweagent/agents/pipit_agent.py   # PipitAgent adapter
├── src/minisweagent/config/benchmarks/pipit_config.yaml
└── run_pipit_bench.py                        # Standalone local runner
```

The `PipitAgent` class wraps pipit as a subprocess, replacing mini-swe-agent's default LLM+bash loop with pipit's own agent architecture.

---

## Related Benchmark Results

| Benchmark | File | Model | Summary |
|---|---|---|---|
| E2E (Qwen 3.5-35B local) | [BENCHMARK-E2E-RESULTS.md](BENCHMARK-E2E-RESULTS.md) | Qwen3.5-35B-A3B-FP8 | 4-tier eval, 0.1.0 debug build |
| E2E (Azure GPT-5.4-mini) | [BENCHMARK-AZURE-GPT54-RESULTS.md](BENCHMARK-AZURE-GPT54-RESULTS.md) | gpt-5.4-mini | 6 real-world tasks, 38/39 tests pass, 9.0/10 avg |
| Terminal-bench | [BENCHMARK-TERMINAL-RESULTS.md](BENCHMARK-TERMINAL-RESULTS.md) | Various | Terminal-bench core dataset |
| Fish shell | [BENCHMARK-FISH-RESULTS.md](BENCHMARK-FISH-RESULTS.md) | Various | Fish shell integration |
| Parallel | [BENCHMARK-PARALLEL-RESULTS.md](BENCHMARK-PARALLEL-RESULTS.md) | Various | Parallel execution |
| Render | [BENCHMARK-RENDER-RESULTS.md](BENCHMARK-RENDER-RESULTS.md) | Various | UI rendering |
| Skills | [BENCHMARK-SKILLS-RESULTS.md](BENCHMARK-SKILLS-RESULTS.md) | Various | Skill activation |
| Tools | [BENCHMARK-PIPIT-TOOLS-RESULTS.md](BENCHMARK-PIPIT-TOOLS-RESULTS.md) | Various | Tool usage |
