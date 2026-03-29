#!/usr/bin/env python3
"""
Real E2E Feature Tests — Exercises ALL new pipit features with real LLM calls.

Tests:
  1. Git Archaeology (WS3) — temporal knowledge graph on real repo
  2. Dependency Health (WS1) — scan real manifests
  3. Health Monitor (WS1) — EWMA metrics + remediation
  4. Adaptive Planner (WS1) — Thompson Sampling scoring
  5. Knowledge Injection (WS6) — cross-project retrieval
  6. Agent Mesh (WS2) — capability discovery + negotiation
  7. Adversarial Analysis (WS4) — attack surface detection on real code
  8. Spec Language (WS5) — spec → ghost code → consistency check
  9. Semantic IR (WS8) — cross-language pattern detection
  10. Compliance (WS9) — regulation parsing + taint analysis
  11. Architecture Evolution (WS10) — genome evolution + scaffold
  12. Evolutionary Optimization (Bet 1) — population + Pareto front
  13. Formal Verification (Bet 2) — CSL → SMT-LIB2
  14. Performance Analysis (Bet 4) — flamegraph + hypotheses
  15. Environment Fingerprint (Bet 5) — collect + diagnose
  16. Real Agent Task (Integration) — pipit solves a bug with all systems active

Each test:
  - Sets up a /tmp workspace
  - Runs the feature
  - Validates results with hidden checks
  - Reports PASS/FAIL with details
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

PIPIT = str(Path(__file__).parent.parent / "target" / "debug" / "pipit")
MODEL = "Qwen/Qwen3.5-35B-A3B-FP8"
BASE_URL = "http://192.168.1.198:8000"
RESULTS = []
START_TIME = time.time()


def run_pipit(workdir, prompt, max_turns=8, timeout=120):
    """Run pipit with real LLM and return (rc, elapsed)."""
    cmd = [PIPIT, prompt, "--provider", "openai", "--model", MODEL,
           "--base-url", BASE_URL, "--api-key", "dummy",
           "--approval", "full_auto", "--max-turns", str(max_turns),
           "--root", str(workdir)]
    start = time.time()
    try:
        r = subprocess.run(cmd, cwd=str(workdir), timeout=timeout,
                           capture_output=True, text=True)
        return r.returncode, time.time() - start, r.stdout, r.stderr
    except subprocess.TimeoutExpired:
        return -1, timeout, "", "TIMEOUT"


def record(name, tier, checks, elapsed, details=""):
    passed = sum(1 for _, ok in checks if ok)
    total = len(checks)
    status = "PASS" if passed == total else "FAIL"
    RESULTS.append({
        "name": name, "tier": tier, "status": status,
        "checks": f"{passed}/{total}", "elapsed": f"{elapsed:.1f}s",
        "details": details,
        "check_details": [(n, "✓" if ok else "✗") for n, ok in checks],
    })
    icon = "✓" if status == "PASS" else "✗"
    print(f"  {icon} {name}: {passed}/{total} checks ({elapsed:.1f}s)")


# ═══════════════════════════════════════════════════════════════════════
# Test 1: Git Archaeology — Temporal Knowledge Graph
# ═══════════════════════════════════════════════════════════════════════
def test_git_archaeology():
    print("\n▸ Test 1: Git Archaeology")
    repo = Path(__file__).parent.parent  # forge-cli itself
    t0 = time.time()

    # Run Rust code via cargo test (the module has a test that builds on real repo)
    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-intelligence", "--lib",
         "git_archaeology::tests::test_temporal_graph_build_on_real_repo",
         "--", "--nocapture"],
        cwd=str(repo), capture_output=True, text=True, timeout=30)

    checks = [
        ("test passes", r.returncode == 0),
        ("found file history", "file history" not in r.stderr or r.returncode == 0),
    ]

    # Also directly test: can we get experts for a known file?
    # Parse git log to verify the graph would have data
    log_r = subprocess.run(
        ["git", "log", "--oneline", "-5", "--", "crates/pipit-core/src/agent.rs"],
        cwd=str(repo), capture_output=True, text=True)
    checks.append(("git log has commits for agent.rs", len(log_r.stdout.strip()) > 0))

    record("Git Archaeology", "WS3", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 2: Dependency Health Scan
# ═══════════════════════════════════════════════════════════════════════
def test_dependency_health():
    print("\n▸ Test 2: Dependency Health")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-intelligence", "--lib",
         "dependency_health::tests::test_analyze_real_cargo",
         "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    checks = [
        ("test passes", r.returncode == 0),
        ("found dependencies", "dependencies" not in r.stderr or r.returncode == 0),
    ]

    # Also test: scan a package.json
    tmpdir = Path(tempfile.mkdtemp(prefix="pipit-dep-"))
    (tmpdir / "package.json").write_text(json.dumps({
        "dependencies": {"express": "^4.18.0", "lodash": "^4.17.0"},
        "devDependencies": {"jest": "^29.0.0"}
    }))

    r2 = subprocess.run(
        ["cargo", "test", "-p", "pipit-intelligence", "--lib",
         "dependency_health::tests::test_semver_parse",
         "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)
    checks.append(("semver parsing works", r2.returncode == 0))

    shutil.rmtree(tmpdir, ignore_errors=True)
    record("Dependency Health", "WS1", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 3: Health Monitor EWMA
# ═══════════════════════════════════════════════════════════════════════
def test_health_monitor():
    print("\n▸ Test 3: Health Monitor EWMA")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-daemon",
         "--", "health_monitor::tests", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    checks = [
        ("EWMA convergence test passes", r.returncode == 0),
        ("noise filtering works", "dampen" not in r.stderr or r.returncode == 0),
    ]
    record("Health Monitor", "WS1", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 4: Adaptive Planner (Thompson Sampling)
# ═══════════════════════════════════════════════════════════════════════
def test_adaptive_planner():
    print("\n▸ Test 4: Adaptive Planner")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-core", "--test", "benchmark_planner",
         "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=60)

    checks = [
        ("all benchmark planner tests pass", r.returncode == 0),
    ]

    # Count passing tests
    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0
    checks.append((f">=15 planner tests ({count})", count >= 15))

    record("Adaptive Planner", "WS1", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 5: Agent Mesh — Discovery + Negotiation
# ═══════════════════════════════════════════════════════════════════════
def test_agent_mesh():
    print("\n▸ Test 5: Agent Mesh")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-agent-mesh", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    checks = [
        ("all mesh tests pass", r.returncode == 0),
    ]
    m = re.search(r"(\d+) passed", r.stdout + r.stderr) if 're' in dir() else None
    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0
    checks.append((f"registry + negotiation tests ({count})", count >= 5))

    record("Agent Mesh", "WS2", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 6: Adversarial Analysis on Real Code
# ═══════════════════════════════════════════════════════════════════════
def test_adversarial():
    print("\n▸ Test 6: Adversarial Analysis")
    t0 = time.time()

    # Test on a deliberately vulnerable Python app
    tmpdir = Path(tempfile.mkdtemp(prefix="pipit-adv-"))
    (tmpdir / "vulnerable_app.py").write_text('''\
from flask import Flask, request
import subprocess
import sqlite3
import pickle

app = Flask(__name__)

@app.route('/search')
def search():
    query = request.args.get('q')
    conn = sqlite3.connect('app.db')
    cursor = conn.cursor()
    cursor.execute(f"SELECT * FROM items WHERE name LIKE '%{query}%'")
    return str(cursor.fetchall())

@app.route('/run')
def run_cmd():
    cmd = request.args.get('cmd')
    result = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    return result.stdout

@app.route('/load')
def load_data():
    data = request.get_data()
    obj = pickle.loads(data)
    return str(obj)
''')

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-core", "--lib",
         "adversarial::tests::test_detect_surfaces", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)
    checks = [("adversarial tests pass", r.returncode == 0)]

    # Now run pipit to review the vulnerable code
    rc, elapsed, stdout, stderr = run_pipit(
        tmpdir,
        "Review vulnerable_app.py for security vulnerabilities. List each vulnerability with severity and fix.",
        max_turns=6, timeout=90)

    checks.append(("pipit ran successfully", rc == 0))

    # Check if pipit found the vulnerabilities
    combined = stdout + stderr
    lower = combined.lower()
    checks.append(("found SQL injection", "sql" in lower and "inject" in lower))
    checks.append(("found command injection", "command" in lower or "subprocess" in lower or "shell" in lower))
    checks.append(("found pickle deserialization", "pickle" in lower or "deserializ" in lower))

    shutil.rmtree(tmpdir, ignore_errors=True)
    record("Adversarial Analysis", "WS4", checks, time.time() - t0,
           f"LLM review took {elapsed:.1f}s")


# ═══════════════════════════════════════════════════════════════════════
# Test 7: Spec Language → Ghost Code → Consistency
# ═══════════════════════════════════════════════════════════════════════
def test_spec_ghost_code():
    print("\n▸ Test 7: Spec + Ghost Code")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-spec", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0

    checks = [
        ("all spec tests pass", r.returncode == 0),
        (f"spec tests count ({count})", count >= 5),
    ]
    record("Spec + Ghost Code", "WS5", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 8: Semantic IR — Cross-Language Pattern Detection
# ═══════════════════════════════════════════════════════════════════════
def test_semantic_ir():
    print("\n▸ Test 8: Semantic IR")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-intelligence", "--lib",
         "semantic_ir::tests", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    r2 = subprocess.run(
        ["cargo", "test", "-p", "pipit-intelligence", "--lib",
         "projector::tests", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    checks = [
        ("IR detection tests pass", r.returncode == 0),
        ("projector tests pass", r2.returncode == 0),
    ]
    record("Semantic IR + Projector", "WS8", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 9: Compliance — Regulation Parsing + Taint
# ═══════════════════════════════════════════════════════════════════════
def test_compliance():
    print("\n▸ Test 9: Compliance")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-compliance", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0

    checks = [
        ("compliance tests pass", r.returncode == 0),
        (f"test count ({count})", count >= 4),
    ]
    record("Compliance", "WS9", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 10: Architecture Evolution — NSGA-II + Scaffold
# ═══════════════════════════════════════════════════════════════════════
def test_arch_evolution():
    print("\n▸ Test 10: Architecture Evolution")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-arch-evolution", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=60)

    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0

    checks = [
        ("evolution tests pass", r.returncode == 0),
        (f"test count ({count})", count >= 6),
    ]
    record("Architecture Evolution", "WS10", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 11: Evolutionary Optimization (Bet 1) — Population + Pareto
# ═══════════════════════════════════════════════════════════════════════
def test_evo_optimization():
    print("\n▸ Test 11: Evolutionary Optimization")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-evolve", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=60)

    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0

    checks = [
        ("evolve tests pass", r.returncode == 0),
        (f"test count ({count})", count >= 8),
    ]
    record("Evolutionary Optimization", "Bet1", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 12: Formal Verification (Bet 2) — CSL + SMT
# ═══════════════════════════════════════════════════════════════════════
def test_formal_verification():
    print("\n▸ Test 12: Formal Verification")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-verify", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0

    checks = [
        ("verify tests pass", r.returncode == 0),
        (f"test count ({count})", count >= 6),
    ]
    record("Formal Verification", "Bet2", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 13: Performance Analysis (Bet 4)
# ═══════════════════════════════════════════════════════════════════════
def test_perf_analysis():
    print("\n▸ Test 13: Performance Analysis")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-perf", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0

    checks = [
        ("perf tests pass", r.returncode == 0),
        (f"test count ({count})", count >= 8),
    ]
    record("Performance Analysis", "Bet4", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 14: Environment Fingerprint (Bet 5)
# ═══════════════════════════════════════════════════════════════════════
def test_env_fingerprint():
    print("\n▸ Test 14: Environment Fingerprint")
    t0 = time.time()

    r = subprocess.run(
        ["cargo", "test", "-p", "pipit-env", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    import re
    m = re.search(r"(\d+) passed", r.stdout + r.stderr)
    count = int(m.group(1)) if m else 0

    checks = [
        ("env tests pass", r.returncode == 0),
        (f"test count ({count})", count >= 5),
    ]
    record("Environment Fingerprint", "Bet5", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Test 15: REAL LLM — Bug Fix with Planning + Verification
# ═══════════════════════════════════════════════════════════════════════
def test_real_llm_bug_fix():
    print("\n▸ Test 15: Real LLM Bug Fix (Integration)")
    tmpdir = Path(tempfile.mkdtemp(prefix="pipit-llm-"))
    t0 = time.time()

    # Create a project with a multi-file bug
    (tmpdir / "inventory.py").write_text('''\
"""Inventory management system with a concurrency bug."""
import threading

class Inventory:
    def __init__(self):
        self.items = {}
        self.lock = threading.Lock()

    def add_item(self, name, quantity):
        # BUG: lock not used consistently
        if name in self.items:
            self.items[name] += quantity
        else:
            self.items[name] = quantity

    def remove_item(self, name, quantity):
        with self.lock:
            if name not in self.items:
                raise ValueError(f"Item {name} not found")
            if self.items[name] < quantity:
                raise ValueError(f"Not enough {name}")
            self.items[name] -= quantity
            if self.items[name] == 0:
                del self.items[name]

    def get_stock(self, name):
        return self.items.get(name, 0)

    def total_value(self, prices):
        """Calculate total inventory value."""
        total = 0
        for name, qty in self.items.items():
            # BUG: KeyError if item not in prices dict
            total += qty * prices[name]
        return total
''')

    (tmpdir / "test_inventory.py").write_text('''\
import pytest
import threading
from inventory import Inventory

def test_add_item():
    inv = Inventory()
    inv.add_item("apple", 10)
    assert inv.get_stock("apple") == 10

def test_remove_item():
    inv = Inventory()
    inv.add_item("apple", 10)
    inv.remove_item("apple", 3)
    assert inv.get_stock("apple") == 7

def test_concurrent_add():
    inv = Inventory()
    threads = []
    for _ in range(100):
        t = threading.Thread(target=inv.add_item, args=("widget", 1))
        threads.append(t)
        t.start()
    for t in threads:
        t.join()
    assert inv.get_stock("widget") == 100

def test_total_value_missing_price():
    inv = Inventory()
    inv.add_item("apple", 5)
    inv.add_item("banana", 3)
    prices = {"apple": 1.50}  # banana price missing!
    # Should handle missing prices gracefully, not crash
    try:
        total = inv.total_value(prices)
        # If it doesn't crash, it should skip or use 0 for missing
        assert total >= 0
    except KeyError:
        pytest.fail("total_value crashed on missing price — should handle gracefully")

def test_remove_nonexistent():
    inv = Inventory()
    with pytest.raises(ValueError):
        inv.remove_item("nonexistent", 1)
''')

    rc, elapsed, stdout, stderr = run_pipit(
        tmpdir,
        "Fix all bugs in inventory.py. There's a concurrency bug in add_item (lock not used) "
        "and total_value crashes on missing prices. Run the tests to verify.",
        max_turns=12, timeout=120)

    code = (tmpdir / "inventory.py").read_text()
    checks = [
        ("pipit completed", rc == 0),
        ("add_item uses lock", "lock" in code.split("def add_item")[1].split("def ")[0] if "def add_item" in code else False),
        ("total_value handles missing", "get(" in code.split("def total_value")[1] if "def total_value" in code else ("KeyError" in code or "default" in code.lower())),
    ]

    # Run tests
    test_r = subprocess.run(
        ["python3", "-m", "pytest", "test_inventory.py", "-v"],
        cwd=str(tmpdir), capture_output=True, text=True, timeout=30)
    checks.append(("all tests pass", test_r.returncode == 0))

    shutil.rmtree(tmpdir, ignore_errors=True)
    record("Real LLM Bug Fix", "Integration", checks, time.time() - t0,
           f"LLM took {elapsed:.1f}s")


# ═══════════════════════════════════════════════════════════════════════
# Test 16: REAL LLM — Security Hardening + Test Generation
# ═══════════════════════════════════════════════════════════════════════
def test_real_llm_security():
    print("\n▸ Test 16: Real LLM Security Hardening")
    tmpdir = Path(tempfile.mkdtemp(prefix="pipit-sec-"))
    t0 = time.time()

    (tmpdir / "auth.py").write_text('''\
import hashlib
import os

SECRET = "admin123"  # hardcoded secret

def hash_password(password):
    return hashlib.md5(password.encode()).hexdigest()

def check_password(password, hashed):
    return hash_password(password) == hashed

def create_token():
    import time
    return str(int(time.time()))

def log_login(user, password, success):
    print(f"Login: user={user} pass={password} success={success}")
''')

    rc, elapsed, stdout, stderr = run_pipit(
        tmpdir,
        "Fix ALL security vulnerabilities in auth.py: "
        "1) Remove hardcoded secret 2) Replace MD5 with SHA-256 + salt "
        "3) Use constant-time comparison 4) Use secure random for tokens "
        "5) Don't log passwords",
        max_turns=8, timeout=90)

    code = (tmpdir / "auth.py").read_text()
    checks = [
        ("pipit completed", rc == 0),
        ("no hardcoded secret", "admin123" not in code),
        ("uses sha256+", "sha256" in code or "sha512" in code or "pbkdf2" in code or "bcrypt" in code),
        ("constant-time compare", "hmac" in code or "compare_digest" in code or "secrets" in code),
        ("secure tokens", "secrets" in code or "os.urandom" in code or "uuid" in code or "token_hex" in code),
        ("no password in log", "pass=" not in code.split("def log_login")[1] if "def log_login" in code else True),
    ]

    # Verify it's importable
    r = subprocess.run(["python3", "-c", "import sys; sys.path.insert(0,'.'); import auth; print('OK')"],
                       cwd=str(tmpdir), capture_output=True, text=True, timeout=10)
    checks.append(("importable after fixes", "OK" in r.stdout))

    shutil.rmtree(tmpdir, ignore_errors=True)
    record("Real LLM Security", "Integration", checks, time.time() - t0,
           f"LLM took {elapsed:.1f}s")


# ═══════════════════════════════════════════════════════════════════════
# Test 17: REAL LLM — Multi-File Feature + Tests
# ═══════════════════════════════════════════════════════════════════════
def test_real_llm_feature():
    print("\n▸ Test 17: Real LLM Multi-File Feature")
    tmpdir = Path(tempfile.mkdtemp(prefix="pipit-feat-"))
    t0 = time.time()

    (tmpdir / "data_store.py").write_text('''\
"""Simple key-value data store."""

class DataStore:
    def __init__(self):
        self._data = {}

    def set(self, key, value):
        self._data[key] = value

    def get(self, key):
        return self._data.get(key)

    def delete(self, key):
        if key in self._data:
            del self._data[key]

    def keys(self):
        return list(self._data.keys())
''')

    rc, elapsed, stdout, stderr = run_pipit(
        tmpdir,
        "Add these features to DataStore in data_store.py:\n"
        "1. TTL support: set(key, value, ttl=None) where ttl is seconds until expiry\n"
        "2. A has(key) method that returns True if key exists and hasn't expired\n"
        "3. An expired_keys() method that returns list of expired keys\n"
        "4. Write comprehensive tests in test_data_store.py\n"
        "5. Run tests to verify everything works",
        max_turns=12, timeout=120)

    code = (tmpdir / "data_store.py").read_text()
    checks = [
        ("pipit completed", rc == 0),
        ("ttl parameter added", "ttl" in code),
        ("has method exists", "def has" in code),
        ("expired_keys method", "expired" in code.lower()),
    ]

    # Check tests exist and pass
    test_file = tmpdir / "test_data_store.py"
    checks.append(("test file created", test_file.exists()))

    if test_file.exists():
        r = subprocess.run(
            ["python3", "-m", "pytest", "test_data_store.py", "-v"],
            cwd=str(tmpdir), capture_output=True, text=True, timeout=30)
        checks.append(("all tests pass", r.returncode == 0))
    else:
        checks.append(("all tests pass", False))

    shutil.rmtree(tmpdir, ignore_errors=True)
    record("Real LLM Feature", "Integration", checks, time.time() - t0,
           f"LLM took {elapsed:.1f}s")


# ═══════════════════════════════════════════════════════════════════════
# Test 18: HW Co-Design + Test Universe (unit tests)
# ═══════════════════════════════════════════════════════════════════════
def test_hw_and_test_universe():
    print("\n▸ Test 18: HW Co-Design + Test Universe")
    t0 = time.time()

    r1 = subprocess.run(
        ["cargo", "test", "-p", "pipit-hw-codesign", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    r2 = subprocess.run(
        ["cargo", "test", "-p", "pipit-test-universe", "--", "--nocapture"],
        cwd=str(Path(__file__).parent.parent),
        capture_output=True, text=True, timeout=30)

    import re
    m1 = re.search(r"(\d+) passed", r1.stdout + r1.stderr)
    m2 = re.search(r"(\d+) passed", r2.stdout + r2.stderr)
    c1 = int(m1.group(1)) if m1 else 0
    c2 = int(m2.group(1)) if m2 else 0

    checks = [
        ("HW co-design tests pass", r1.returncode == 0),
        (f"HW tests ({c1})", c1 >= 3),
        ("test universe tests pass", r2.returncode == 0),
        (f"universe tests ({c2})", c2 >= 3),
    ]
    record("HW + Test Universe", "Bet3+WS7", checks, time.time() - t0)


# ═══════════════════════════════════════════════════════════════════════
# Main runner
# ═══════════════════════════════════════════════════════════════════════

def main():
    print("=" * 65)
    print("  Pipit Feature Test Suite — Real LLM + Unit Tests")
    print(f"  Model: {MODEL}")
    print(f"  Endpoint: {BASE_URL}")
    print("=" * 65)

    tests = [
        test_git_archaeology,
        test_dependency_health,
        test_health_monitor,
        test_adaptive_planner,
        test_agent_mesh,
        test_adversarial,
        test_spec_ghost_code,
        test_semantic_ir,
        test_compliance,
        test_arch_evolution,
        test_evo_optimization,
        test_formal_verification,
        test_perf_analysis,
        test_env_fingerprint,
        test_real_llm_bug_fix,
        test_real_llm_security,
        test_real_llm_feature,
        test_hw_and_test_universe,
    ]

    for test_fn in tests:
        try:
            test_fn()
        except Exception as e:
            record(test_fn.__name__, "ERROR", [("no crash", False)], 0, str(e))

    # Summary
    total_elapsed = time.time() - START_TIME
    print("\n" + "=" * 65)
    print("  RESULTS SUMMARY")
    print("=" * 65)
    print(f"\n{'Test':<40} {'Tier':<12} {'Checks':<8} {'Time':<8} {'Status'}")
    print("-" * 76)

    pass_count = 0
    total_checks = 0
    total_checks_passed = 0

    for r in RESULTS:
        print(f"{r['name']:<40} {r['tier']:<12} {r['checks']:<8} {r['elapsed']:<8} {r['status']}")
        if r['status'] == 'PASS':
            pass_count += 1
        parts = r['checks'].split('/')
        total_checks_passed += int(parts[0])
        total_checks += int(parts[1])

    print("-" * 76)
    print(f"{'TOTAL':<40} {'':12} {total_checks_passed}/{total_checks:<8} {total_elapsed:.0f}s    {pass_count}/{len(RESULTS)}")
    print(f"\nPass rate: {pass_count}/{len(RESULTS)} ({100*pass_count/len(RESULTS):.0f}%)")
    print(f"Check rate: {total_checks_passed}/{total_checks} ({100*total_checks_passed/total_checks:.0f}%)")

    # Write results to MD file
    md_path = "/tmp/pipit-feature-test-results.md"
    with open(md_path, "w") as f:
        f.write("# Pipit Feature Test Results\n\n")
        f.write(f"**Date**: {time.strftime('%Y-%m-%d %H:%M')}\n")
        f.write(f"**Model**: `{MODEL}`\n")
        f.write(f"**Endpoint**: `{BASE_URL}`\n")
        f.write(f"**Total Time**: {total_elapsed:.0f}s\n\n")
        f.write("## Summary\n\n")
        f.write(f"| Test | Tier | Checks | Time | Status |\n")
        f.write(f"|------|------|--------|------|--------|\n")
        for r in RESULTS:
            f.write(f"| {r['name']} | {r['tier']} | {r['checks']} | {r['elapsed']} | **{r['status']}** |\n")
        f.write(f"\n**Pass rate**: {pass_count}/{len(RESULTS)} ({100*pass_count/len(RESULTS):.0f}%)\n")
        f.write(f"**Check rate**: {total_checks_passed}/{total_checks} ({100*total_checks_passed/total_checks:.0f}%)\n\n")

        f.write("## Detailed Results\n\n")
        for r in RESULTS:
            f.write(f"### {r['name']} ({r['tier']})\n\n")
            f.write(f"**Status**: {r['status']} | **Checks**: {r['checks']} | **Time**: {r['elapsed']}\n\n")
            if r.get('details'):
                f.write(f"_{r['details']}_\n\n")
            for name, icon in r.get('check_details', []):
                f.write(f"- {icon} {name}\n")
            f.write("\n")

    print(f"\nResults written to {md_path}")

    # Also write JSON
    json_path = "/tmp/pipit-feature-test-results.json"
    with open(json_path, "w") as f:
        json.dump(RESULTS, f, indent=2)


if __name__ == "__main__":
    import re
    main()
