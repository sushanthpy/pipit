#!/usr/bin/env python3
"""Quick re-test of TT.3 Git Archaeology only."""
import sys, tempfile, shutil, subprocess, time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from run_benchmarks import setup_terminal_git_archaeology, check_terminal_git_archaeology, run_pipit

workdir = Path(tempfile.mkdtemp(prefix="pipit-bench-TT3-retest-"))
print(f"Workdir: {workdir}")
config = setup_terminal_git_archaeology(workdir)
prompt = config["prompt"]
max_turns = config.get("max_turns", 10)
print(f"Prompt: {prompt}")
print(f"Running pipit (max {max_turns} turns)...")
rc, elapsed = run_pipit(workdir, prompt, max_turns)
print(f"Finished in {elapsed:.1f}s (rc={rc})")

code = (workdir / "math_ops.py").read_text()
print("--- math_ops.py ---")
print(code)
print("--- end ---")

checks = check_terminal_git_archaeology(workdir)
passed = sum(1 for _, ok in checks if ok)
total = len(checks)
print(f"Checks: {passed}/{total}")
for name, ok in checks:
    icon = "PASS" if ok else "FAIL"
    print(f"  {icon} {name}")

shutil.rmtree(workdir, ignore_errors=True)
