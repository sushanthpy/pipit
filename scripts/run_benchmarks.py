#!/usr/bin/env python3
"""
Real E2E Benchmark Runner for Pipit CLI Agent.

Creates test workspaces with seeded code, runs pipit against each scenario,
validates results with hidden checks, and scores outcomes.

Usage:
    python3 scripts/run_benchmarks.py
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

# ── Configuration ──
PIPIT_BINARY = os.environ.get("PIPIT_BIN", str(Path(__file__).parent.parent / "target" / "debug" / "pipit"))
PROVIDER = "openai"
MODEL = "Qwen/Qwen3.5-35B-A3B-FP8"
BASE_URL = "http://192.168.1.198:8000"
API_KEY = "dummy"
MAX_TURNS = 15
TIMEOUT = 180  # seconds per test


@dataclass
class BenchResult:
    name: str
    tier: str
    passed: bool
    checks_passed: int
    checks_total: int
    turns: int
    elapsed: float
    details: str = ""
    error: str = ""


# ═══════════════════════════════════════════════════════════════════════════
#  Test Scenario Definitions
# ═══════════════════════════════════════════════════════════════════════════

def setup_t1_bug_fix(workdir: Path):
    """Tier 1, Test 2: Fix 3 bugs in calculator.py"""
    (workdir / "calculator.py").write_text('''\
"""Simple calculator module with some bugs."""

def add(a, b):
    return a + b

def subtract(a, b):
    return a - b

def multiply(a, b):
    return a * b

def divide(a, b):
    # BUG 1: No zero division check
    return a / b

def power(a, b):
    # BUG 2: Wrong implementation (multiplies instead of exponentiating)
    return a * b

def modulo(a, b):
    # BUG 3: Missing check for b == 0
    return a % b
''')
    (workdir / "test_calculator.py").write_text('''\
import pytest
from calculator import add, subtract, multiply, divide, power, modulo

def test_add():
    assert add(2, 3) == 5
    assert add(-1, 1) == 0

def test_subtract():
    assert subtract(5, 3) == 2

def test_multiply():
    assert multiply(3, 4) == 12

def test_divide():
    assert divide(10, 2) == 5.0
    with pytest.raises((ZeroDivisionError, ValueError)):
        divide(10, 0)

def test_power():
    assert power(2, 3) == 8
    assert power(5, 0) == 1
    assert power(3, 2) == 9

def test_modulo():
    assert modulo(10, 3) == 1
    with pytest.raises((ZeroDivisionError, ValueError)):
        modulo(10, 0)
''')
    return {
        "prompt": "Fix all the bugs in calculator.py. Run the tests to verify your fixes pass.",
        "max_turns": 10,
    }


def check_t1_bug_fix(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for T1 bug fix."""
    checks = []
    calc = (workdir / "calculator.py").read_text()

    # Check 1: divide handles zero
    checks.append(("divide handles zero division", "if" in calc.split("def divide")[1].split("def ")[0] if "def divide" in calc else False))

    # Check 2: power uses ** not *
    checks.append(("power uses exponentiation", "**" in calc.split("def power")[1].split("def ")[0] if "def power" in calc else False))

    # Check 3: modulo handles zero
    checks.append(("modulo handles zero division", "if" in calc.split("def modulo")[1] if "def modulo" in calc else False))

    # Check 4: tests actually pass
    result = subprocess.run(
        ["python3", "-m", "pytest", "test_calculator.py", "-v"],
        cwd=str(workdir), capture_output=True, text=True, timeout=30,
    )
    checks.append(("all tests pass", result.returncode == 0))

    # Check 5: no other functions modified
    checks.append(("add unchanged", "return a + b" in calc))
    checks.append(("subtract unchanged", "return a - b" in calc))

    return checks


def setup_t1_file_creation(workdir: Path):
    """Tier 1, Test 1: Create a helper module from scratch."""
    return {
        "prompt": (
            "Create a Python file called string_utils.py with these 5 functions:\n"
            "1. reverse_string(s) - reverse a string\n"
            "2. count_vowels(s) - count vowels (a,e,i,o,u) case-insensitive\n"
            "3. is_palindrome(s) - check if string is palindrome (case-insensitive, ignore spaces)\n"
            "4. truncate(s, max_len) - truncate to max_len chars, add '...' if truncated\n"
            "5. snake_to_camel(s) - convert snake_case to camelCase\n"
            "Include type hints and docstrings."
        ),
        "max_turns": 8,
    }


def check_t1_file_creation(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for file creation."""
    checks = []
    path = workdir / "string_utils.py"
    checks.append(("file exists", path.exists()))
    if not path.exists():
        return checks + [("importable", False), ("reverse works", False),
                         ("vowels works", False), ("palindrome works", False),
                         ("truncate works", False), ("snake_to_camel works", False)]

    content = path.read_text()
    checks.append(("has type hints", "->" in content))

    # Try importing and running
    test_code = '''\
import sys
sys.path.insert(0, ".")
from string_utils import reverse_string, count_vowels, is_palindrome, truncate, snake_to_camel

assert reverse_string("hello") == "olleh", f"reverse: {reverse_string('hello')}"
assert count_vowels("Hello World") == 3, f"vowels: {count_vowels('Hello World')}"
assert is_palindrome("racecar") == True
assert is_palindrome("Race Car") == True
assert is_palindrome("hello") == False
assert truncate("hello world", 5) == "he...", f"truncate: {truncate('hello world', 5)}"
assert truncate("hi", 10) == "hi"
assert snake_to_camel("hello_world") == "helloWorld", f"snake: {snake_to_camel('hello_world')}"
assert snake_to_camel("my_var_name") == "myVarName"
print("ALL_CHECKS_PASSED")
'''
    result = subprocess.run(
        ["python3", "-c", test_code],
        cwd=str(workdir), capture_output=True, text=True, timeout=10,
    )
    checks.append(("all functions work", "ALL_CHECKS_PASSED" in result.stdout))
    if "ALL_CHECKS_PASSED" not in result.stdout:
        checks.append(("error details", False))
        # Try individual checks
        for fn_name in ["reverse_string", "count_vowels", "is_palindrome", "truncate", "snake_to_camel"]:
            try:
                r = subprocess.run(
                    ["python3", "-c", f'import sys; sys.path.insert(0,"."); from string_utils import {fn_name}; print("OK")'],
                    cwd=str(workdir), capture_output=True, text=True, timeout=5,
                )
                checks.append((f"{fn_name} importable", "OK" in r.stdout))
            except Exception:
                checks.append((f"{fn_name} importable", False))

    return checks


def setup_t1_test_generation(workdir: Path):
    """Tier 1, Test 5: Write tests for existing code."""
    (workdir / "mathlib.py").write_text('''\
"""Math utility library."""
import math

def factorial(n: int) -> int:
    """Calculate factorial of n. Raises ValueError for negative n."""
    if n < 0:
        raise ValueError("Factorial not defined for negative numbers")
    if n <= 1:
        return 1
    return n * factorial(n - 1)

def fibonacci(n: int) -> list[int]:
    """Return first n Fibonacci numbers."""
    if n <= 0:
        return []
    if n == 1:
        return [0]
    seq = [0, 1]
    for _ in range(2, n):
        seq.append(seq[-1] + seq[-2])
    return seq

def is_prime(n: int) -> bool:
    """Check if n is prime."""
    if n < 2:
        return False
    for i in range(2, int(math.sqrt(n)) + 1):
        if n % i == 0:
            return False
    return True

def gcd(a: int, b: int) -> int:
    """Greatest common divisor using Euclidean algorithm."""
    while b:
        a, b = b, a % b
    return abs(a)
''')
    return {
        "prompt": (
            "Write comprehensive pytest tests for mathlib.py. "
            "Cover normal cases, edge cases (0, 1, negative), and error handling. "
            "Save as test_mathlib.py. Run the tests to make sure they all pass."
        ),
        "max_turns": 12,
    }


def check_t1_test_generation(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for test generation."""
    checks = []
    test_file = workdir / "test_mathlib.py"
    checks.append(("test file exists", test_file.exists()))
    if not test_file.exists():
        return checks + [("tests run", False)]

    content = test_file.read_text()
    checks.append(("tests factorial", "factorial" in content))
    checks.append(("tests fibonacci", "fibonacci" in content))
    checks.append(("tests is_prime", "is_prime" in content or "prime" in content))
    checks.append(("tests gcd", "gcd" in content))
    checks.append(("tests negative input", "ValueError" in content or "raises" in content or "negative" in content))
    checks.append(("tests edge case 0", "0" in content))

    result = subprocess.run(
        ["python3", "-m", "pytest", "test_mathlib.py", "-v"],
        cwd=str(workdir), capture_output=True, text=True, timeout=30,
    )
    checks.append(("all tests pass", result.returncode == 0))
    # Count test count
    if "passed" in result.stdout:
        import re
        m = re.search(r"(\d+) passed", result.stdout)
        count = int(m.group(1)) if m else 0
        checks.append((f">=8 tests written ({count})", count >= 8))

    return checks


def setup_t2_hidden_edge_cases(workdir: Path):
    """Tier 2, Test 9: Find and fix hidden edge cases in pagination."""
    (workdir / "paginator.py").write_text('''\
"""Pagination utility."""

def paginate(items: list, page: int, per_page: int) -> dict:
    """Return paginated results.

    Args:
        items: List of items to paginate
        page: Page number (1-indexed)
        per_page: Items per page

    Returns:
        Dict with 'items', 'page', 'per_page', 'total', 'pages'
    """
    total = len(items)
    pages = (total + per_page - 1) // per_page
    start = (page - 1) * per_page
    end = start + per_page
    return {
        "items": items[start:end],
        "page": page,
        "per_page": per_page,
        "total": total,
        "pages": pages,
    }
''')
    (workdir / "test_paginator.py").write_text('''\
from paginator import paginate

def test_basic_pagination():
    items = list(range(1, 11))
    result = paginate(items, 1, 3)
    assert result["items"] == [1, 2, 3]
    assert result["total"] == 10
    assert result["pages"] == 4

def test_last_page():
    items = list(range(1, 11))
    result = paginate(items, 4, 3)
    assert result["items"] == [10]
''')
    return {
        "prompt": (
            "The paginator.py module has hidden edge case bugs. "
            "Test with: page=0, negative page numbers, per_page=0, empty list, "
            "and page beyond total pages. Fix all bugs so the function handles "
            "these edge cases gracefully. Run tests to verify."
        ),
        "max_turns": 12,
    }


def check_t2_hidden_edge_cases(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for edge case fixes."""
    checks = []
    code = (workdir / "paginator.py").read_text()

    # Run edge case tests
    test_code = '''\
import sys
sys.path.insert(0, ".")
from paginator import paginate

# Edge 1: page=0 should not crash, return page 1 or raise ValueError
try:
    r = paginate(list(range(10)), 0, 5)
    # Should either return page 1 items or empty
    ok = isinstance(r, dict) and "items" in r
except (ValueError, IndexError):
    ok = True
print(f"page_zero: {ok}")

# Edge 2: negative page
try:
    r = paginate(list(range(10)), -1, 5)
    ok = isinstance(r, dict) and "items" in r
except (ValueError, IndexError):
    ok = True
print(f"negative_page: {ok}")

# Edge 3: per_page=0 should not crash (ZeroDivisionError)
try:
    r = paginate(list(range(10)), 1, 0)
    ok = isinstance(r, dict)
except (ValueError, ZeroDivisionError):
    ok = True
print(f"per_page_zero: {ok}")

# Edge 4: empty list
try:
    r = paginate([], 1, 5)
    ok = r["items"] == [] and r["total"] == 0
except Exception:
    ok = False
print(f"empty_list: {ok}")

# Edge 5: page beyond total
try:
    r = paginate(list(range(5)), 100, 5)
    ok = r["items"] == []
except Exception:
    ok = False
print(f"beyond_total: {ok}")
'''
    result = subprocess.run(
        ["python3", "-c", test_code],
        cwd=str(workdir), capture_output=True, text=True, timeout=10,
    )
    for line in result.stdout.strip().split("\n"):
        if ": " in line:
            name, val = line.split(": ", 1)
            checks.append((name.strip(), val.strip() == "True"))

    return checks


def setup_t3_minimal_diff(workdir: Path):
    """Tier 3, Test 18: Minimal diff fix."""
    (workdir / "stats.py").write_text('''\
"""Statistics module."""
import math

def mean(values: list[float]) -> float:
    """Calculate arithmetic mean."""
    if not values:
        raise ValueError("Cannot compute mean of empty list")
    return sum(values) / len(values)

def median(values: list[float]) -> float:
    """Calculate median value."""
    if not values:
        raise ValueError("Cannot compute median of empty list")
    sorted_vals = sorted(values)
    n = len(sorted_vals)
    mid = n // 2
    if n % 2 == 0:
        return (sorted_vals[mid - 1] + sorted_vals[mid]) / 2
    return sorted_vals[mid]

def std_dev(values: list[float]) -> float:
    """Calculate population standard deviation."""
    if not values:
        raise ValueError("Cannot compute std_dev of empty list")
    avg = mean(values)
    variance = sum((x - avg) ** 2 for x in values) / len(values)
    return math.sqrt(variance)

def percentile(values: list[float], p: float) -> float:
    """Calculate p-th percentile (0-100)."""
    if not values:
        raise ValueError("Cannot compute percentile of empty list")
    if not (0 <= p <= 100):
        raise ValueError("Percentile must be between 0 and 100")
    sorted_vals = sorted(values)
    n = len(sorted_vals)
    # BUG: should be (n - 1) not n for 0-indexed
    k = (p / 100) * n
    f = math.floor(k)
    c = math.ceil(k)
    if f == c or c >= n:
        return sorted_vals[min(f, n - 1)]
    return sorted_vals[f] * (c - k) + sorted_vals[c] * (k - f)
''')
    (workdir / "test_stats.py").write_text('''\
import pytest
from stats import mean, median, std_dev, percentile

def test_mean():
    assert mean([1, 2, 3, 4, 5]) == 3.0

def test_median_odd():
    assert median([1, 2, 3]) == 2.0

def test_median_even():
    assert median([1, 2, 3, 4]) == 2.5

def test_std_dev():
    assert abs(std_dev([2, 4, 4, 4, 5, 5, 7, 9]) - 2.0) < 0.01

def test_percentile_50():
    assert percentile([1, 2, 3, 4, 5], 50) == 3.0

def test_percentile_0():
    assert percentile([1, 2, 3, 4, 5], 0) == 1.0

def test_percentile_100():
    assert percentile([1, 2, 3, 4, 5], 100) == 5.0

def test_percentile_25():
    # The 25th percentile of [1,2,3,4,5] should be 2.0
    result = percentile([1, 2, 3, 4, 5], 25)
    assert result == 2.0, f"Expected 2.0, got {result}"

def test_empty_raises():
    with pytest.raises(ValueError):
        mean([])
''')
    return {
        "prompt": (
            "There is a bug in the percentile() function in stats.py. "
            "Run the tests to find it, then fix it with the MINIMAL possible change. "
            "Do NOT modify any other function."
        ),
        "max_turns": 8,
    }


def check_t3_minimal_diff(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for minimal diff."""
    checks = []
    code = (workdir / "stats.py").read_text()

    # Check tests pass
    result = subprocess.run(
        ["python3", "-m", "pytest", "test_stats.py", "-v"],
        cwd=str(workdir), capture_output=True, text=True, timeout=30,
    )
    checks.append(("all tests pass", result.returncode == 0))

    # Check other functions unchanged
    checks.append(("mean unchanged", "return sum(values) / len(values)" in code))
    checks.append(("median unchanged", "return sorted_vals[mid]" in code))
    checks.append(("std_dev unchanged", "return math.sqrt(variance)" in code))

    # Check the fix is in percentile
    checks.append(("percentile fixed", "(n - 1)" in code.split("def percentile")[1] if "def percentile" in code else False))

    return checks


def setup_terminal_csv_json(workdir: Path):
    """Terminal T2: CSV to filtered JSON."""
    csv_content = "name,department,salary,status\n"
    employees = [
        ("Alice", "Engineering", 95000, "active"),
        ("Bob", "Marketing", 72000, "active"),
        ("Carol", "Engineering", 105000, "active"),
        ("Dave", "Sales", 68000, "inactive"),
        ("Eve", "Engineering", 88000, "inactive"),
        ("Frank", "HR", 75000, "active"),
        ("Grace", "Engineering", 112000, "active"),
        ("Hank", "Sales", 71000, "active"),
        ("Iris", "Engineering", 99000, "active"),
        ("Jack", "Marketing", 78000, "active"),
    ]
    for name, dept, sal, status in employees:
        csv_content += f"{name},{dept},{sal},{status}\n"
    (workdir / "employees.csv").write_text(csv_content)
    return {
        "prompt": (
            "Read employees.csv and create a JSON file called filtered.json "
            "containing only Engineering department employees who are active. "
            "The JSON should have fields: name, department, salary (as integer), status. "
            "Output should be a JSON array."
        ),
        "max_turns": 8,
    }


def check_terminal_csv_json(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for CSV→JSON."""
    checks = []
    path = workdir / "filtered.json"
    checks.append(("output file exists", path.exists()))
    if not path.exists():
        return checks + [("valid json", False), ("correct count", False), ("correct filter", False)]

    try:
        data = json.loads(path.read_text())
        checks.append(("valid json", True))
    except json.JSONDecodeError:
        checks.append(("valid json", False))
        return checks

    if isinstance(data, dict) and len(data) == 1:
        data = list(data.values())[0]

    checks.append(("is array", isinstance(data, list)))
    if not isinstance(data, list):
        return checks

    checks.append(("correct count (4)", len(data) == 4))
    names = {e.get("name") for e in data}
    checks.append(("has Alice", "Alice" in names))
    checks.append(("has Carol", "Carol" in names))
    checks.append(("has Grace", "Grace" in names))
    checks.append(("has Iris", "Iris" in names))
    checks.append(("no inactive", all(e.get("status") == "active" for e in data)))

    # Check salary is int not string
    if data:
        sal = data[0].get("salary")
        checks.append(("salary is int", isinstance(sal, int)))

    return checks


def setup_terminal_git_archaeology(workdir: Path):
    """Terminal T3: Git archaeology to find bug."""
    # Create a git repo with history
    subprocess.run(["git", "init"], cwd=str(workdir), capture_output=True)
    subprocess.run(["git", "config", "user.email", "test@test.com"], cwd=str(workdir), capture_output=True)
    subprocess.run(["git", "config", "user.name", "Test"], cwd=str(workdir), capture_output=True)

    # Commit 1: original correct code
    (workdir / "math_ops.py").write_text('''\
def integer_divide(a: int, b: int) -> int:
    """Integer division."""
    if b == 0:
        raise ValueError("Division by zero")
    return a // b

def safe_sqrt(n: float) -> float:
    """Square root with validation."""
    if n < 0:
        raise ValueError("Cannot sqrt negative number")
    return n ** 0.5
''')
    subprocess.run(["git", "add", "."], cwd=str(workdir), capture_output=True)
    subprocess.run(["git", "commit", "-m", "Initial correct implementation"], cwd=str(workdir), capture_output=True)

    # Commit 2: introduce bug (// -> /)
    (workdir / "math_ops.py").write_text('''\
def integer_divide(a: int, b: int) -> int:
    """Integer division."""
    if b == 0:
        raise ValueError("Division by zero")
    return a / b

def safe_sqrt(n: float) -> float:
    """Square root with validation."""
    if n < 0:
        raise ValueError("Cannot sqrt negative number")
    return n ** 0.5
''')
    subprocess.run(["git", "add", "."], cwd=str(workdir), capture_output=True)
    subprocess.run(["git", "commit", "-m", "Refactor division for clarity"], cwd=str(workdir), capture_output=True)

    (workdir / "test_math_ops.py").write_text('''\
from math_ops import integer_divide, safe_sqrt

def test_integer_divide():
    assert integer_divide(7, 2) == 3  # Should be 3, not 3.5
    assert integer_divide(10, 3) == 3

def test_safe_sqrt():
    assert safe_sqrt(4) == 2.0
    assert safe_sqrt(0) == 0.0
''')

    return {
        "prompt": (
            "The test_math_ops.py tests are failing — integer_divide returns 3.5 instead of 3. "
            "Use `git log` and `git diff HEAD~1` to find exactly what changed between commits. "
            "Revert the exact operator that was changed. Do NOT modify safe_sqrt."
        ),
        "max_turns": 10,
    }


def check_terminal_git_archaeology(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks."""
    checks = []
    code = (workdir / "math_ops.py").read_text()
    checks.append(("uses integer division", "//" in code.split("def integer_divide")[1].split("def ")[0]))
    checks.append(("safe_sqrt unchanged", "n ** 0.5" in code))

    result = subprocess.run(
        ["python3", "-m", "pytest", "test_math_ops.py", "-v"],
        cwd=str(workdir), capture_output=True, text=True, timeout=30,
    )
    checks.append(("all tests pass", result.returncode == 0))
    return checks


# ═══════════════════════════════════════════════════════════════════════════
#  Runner
# ═══════════════════════════════════════════════════════════════════════════

def setup_t2_security_hardening(workdir: Path):
    """Tier 2, Test 14: Fix security vulnerabilities."""
    (workdir / "auth.py").write_text('''\
"""Authentication module."""
import hashlib
import os
import tempfile

SECRET_KEY = "mysecretkey123"  # BUG 1: Hardcoded secret

def hash_password(password: str) -> str:
    """Hash a password."""
    # BUG 2: Using MD5, no salt
    return hashlib.md5(password.encode()).hexdigest()

def verify_password(password: str, hashed: str) -> bool:
    """Verify password against hash."""
    # BUG 3: Timing attack vulnerable comparison
    return hash_password(password) == hashed

def generate_session_id() -> str:
    """Generate a session ID."""
    # BUG 4: Predictable session IDs
    import time
    return str(int(time.time()))

def save_temp_data(data: str) -> str:
    """Save data to a temp file."""
    # BUG 5: Insecure temp file creation
    path = "/tmp/auth_data.txt"
    with open(path, "w") as f:
        f.write(data)
    return path

def log_auth_attempt(username: str, password: str, success: bool):
    """Log an authentication attempt."""
    # BUG 6: Logging the password!
    print(f"Auth attempt: user={username}, pass={password}, success={success}")
''')
    return {
        "prompt": (
            "This auth.py module has multiple security vulnerabilities. "
            "Fix ALL of them:\n"
            "1. Don't hardcode secrets (use env var or os.urandom)\n"
            "2. Use SHA-256 or bcrypt instead of MD5, add salt\n"
            "3. Use constant-time comparison for password verification\n"
            "4. Use cryptographically secure random for session IDs\n"
            "5. Use tempfile.mkstemp or NamedTemporaryFile for temp files\n"
            "6. Never log passwords\n"
        ),
        "max_turns": 12,
    }


def check_t2_security_hardening(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for security fixes."""
    checks = []
    code = (workdir / "auth.py").read_text()

    checks.append(("no hardcoded secret", 'mysecretkey123' not in code))
    checks.append(("no MD5", "md5" not in code.lower() or "sha" in code.lower()))
    checks.append(("uses sha256 or better", "sha256" in code or "sha512" in code or "bcrypt" in code or "pbkdf2" in code or "scrypt" in code))
    checks.append(("constant-time compare", "hmac" in code or "compare_digest" in code or "secrets.compare_digest" in code))
    checks.append(("secure session ID", "secrets" in code or "os.urandom" in code or "uuid" in code or "token_hex" in code))
    checks.append(("secure temp file", "mkstemp" in code or "NamedTemporaryFile" in code or "mkdtemp" in code))
    checks.append(("no password logging", "pass=" not in code.split("def log_auth")[1] if "def log_auth" in code else True))

    # Check it's still importable
    result = subprocess.run(
        ["python3", "-c", "import sys; sys.path.insert(0,'.'); import auth; print('OK')"],
        cwd=str(workdir), capture_output=True, text=True, timeout=10,
    )
    checks.append(("importable", "OK" in result.stdout))

    return checks


def setup_t3_state_leakage(workdir: Path):
    """Tier 3+, Test 28: Fix test state leakage via conftest.py."""
    (workdir / "cache.py").write_text('''\
"""Simple in-memory cache with global state."""

_cache = {}
_stats = {"hits": 0, "misses": 0}

def get(key: str):
    """Get value from cache."""
    if key in _cache:
        _stats["hits"] += 1
        return _cache[key]
    _stats["misses"] += 1
    return None

def set(key: str, value):
    """Set value in cache."""
    _cache[key] = value

def clear():
    """Clear all cached data."""
    _cache.clear()
    _stats["hits"] = 0
    _stats["misses"] = 0

def stats():
    """Return cache statistics."""
    return dict(_stats)
''')
    (workdir / "test_cache.py").write_text('''\
"""Tests for cache module — these fail when run together due to shared state."""
from cache import get, set, clear, stats

class TestCacheBasic:
    def test_set_and_get(self):
        set("key1", "value1")
        assert get("key1") == "value1"

    def test_get_missing(self):
        # FAILS if run after test_set_and_get — key1 leaks!
        assert get("nonexistent") is None
        s = stats()
        assert s["misses"] == 1, f"Expected 1 miss, got {s}"

    def test_stats_clean(self):
        # FAILS if prior tests ran — stats leak
        s = stats()
        assert s["hits"] == 0 and s["misses"] == 0, f"Expected clean stats, got {s}"

    def test_clear(self):
        set("a", 1)
        set("b", 2)
        clear()
        assert get("a") is None
        assert get("b") is None
''')
    return {
        "prompt": (
            "The tests in test_cache.py fail when run together (state leaks between tests). "
            "Create a conftest.py file that automatically resets the cache state before each test. "
            "Do NOT modify test_cache.py or cache.py. Run pytest to confirm all tests pass."
        ),
        "max_turns": 8,
    }


def check_t3_state_leakage(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for state leakage fix."""
    checks = []

    conftest = workdir / "conftest.py"
    checks.append(("conftest.py exists", conftest.exists()))

    if conftest.exists():
        content = conftest.read_text()
        checks.append(("uses fixture", "fixture" in content or "@pytest.fixture" in content))
        checks.append(("uses autouse", "autouse" in content))
        checks.append(("calls clear", "clear" in content))

    # Verify cache.py and test_cache.py unchanged
    cache_code = (workdir / "cache.py").read_text()
    test_code = (workdir / "test_cache.py").read_text()
    checks.append(("cache.py unmodified", "_cache = {}" in cache_code))
    checks.append(("test_cache.py unmodified", "class TestCacheBasic" in test_code))

    # Run tests
    result = subprocess.run(
        ["python3", "-m", "pytest", "test_cache.py", "-v"],
        cwd=str(workdir), capture_output=True, text=True, timeout=30,
    )
    checks.append(("all tests pass", result.returncode == 0))

    return checks


def setup_t4_error_handling(workdir: Path):
    """Tier 4, Test 27: Error handling audit."""
    (workdir / "processor.py").write_text('''\
"""Data processor with poor error handling."""
import json
import csv

def read_json_file(path: str) -> dict:
    """Read and parse a JSON file."""
    # BUG 1: File handle leak (no with-statement)
    f = open(path, "r")
    data = json.load(f)
    return data

def read_csv_file(path: str) -> list[dict]:
    """Read CSV file into list of dicts."""
    # BUG 2: Same file handle leak
    f = open(path, "r")
    reader = csv.DictReader(f)
    return list(reader)

def convert_format(data: dict, output_format: str) -> str:
    """Convert data to specified format."""
    # BUG 3: No validation of output_format
    if output_format == "json":
        return json.dumps(data, indent=2)
    elif output_format == "csv":
        if not data:
            return ""
        keys = list(data[0].keys()) if isinstance(data, list) else list(data.keys())
        lines = [",".join(keys)]
        items = data if isinstance(data, list) else [data]
        for item in items:
            lines.append(",".join(str(item.get(k, "")) for k in keys))
        return "\\n".join(lines)

def batch_process(file_paths: list[str]) -> dict:
    """Process multiple files, collecting results and errors."""
    results = {}
    # BUG 4: Stops on first error instead of collecting all
    for path in file_paths:
        data = read_json_file(path)
        results[path] = data
    return results
''')
    (workdir / "test_processor.py").write_text('''\
import pytest
import json
import tempfile
import os
from processor import read_json_file, read_csv_file, convert_format, batch_process

def test_read_json():
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
        json.dump({"key": "value"}, f)
        f.flush()
        result = read_json_file(f.name)
    os.unlink(f.name)
    assert result == {"key": "value"}

def test_read_csv():
    with tempfile.NamedTemporaryFile(mode='w', suffix='.csv', delete=False) as f:
        f.write("name,age\\nAlice,30\\nBob,25\\n")
        f.flush()
        result = read_csv_file(f.name)
    os.unlink(f.name)
    assert len(result) == 2
    assert result[0]["name"] == "Alice"

def test_convert_json():
    data = {"name": "test"}
    result = convert_format(data, "json")
    assert json.loads(result) == data

def test_convert_unsupported():
    with pytest.raises(ValueError):
        convert_format({"a": 1}, "xml")

def test_batch_process_with_errors():
    # Create one good file and one bad path
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
        json.dump({"ok": True}, f)
        good_path = f.name

    result = batch_process([good_path, "/nonexistent/file.json"])
    os.unlink(good_path)
    # Should have result for good file and error for bad file
    assert good_path in result
    assert "error" in result.get("/nonexistent/file.json", {}) or isinstance(result.get("/nonexistent/file.json"), dict)
''')
    return {
        "prompt": (
            "Fix the error handling in processor.py:\n"
            "1. Use context managers (with-statements) for all file operations\n"
            "2. Add ValueError for unsupported output formats in convert_format\n"
            "3. Make batch_process collect errors instead of stopping on first failure\n"
            "Run the tests to verify."
        ),
        "max_turns": 12,
    }


def check_t4_error_handling(workdir: Path) -> list[tuple[str, bool]]:
    """Hidden checks for error handling."""
    checks = []
    code = (workdir / "processor.py").read_text()

    checks.append(("uses with-statement in read_json", "with open" in code.split("def read_json")[1].split("def ")[0] if "def read_json" in code else False))
    checks.append(("uses with-statement in read_csv", "with open" in code.split("def read_csv")[1].split("def ")[0] if "def read_csv" in code else False))
    checks.append(("raises ValueError for bad format", "ValueError" in code.split("def convert_format")[1].split("def ")[0] if "def convert_format" in code else False))
    checks.append(("batch_process handles errors", "except" in code.split("def batch_process")[1] if "def batch_process" in code else False or "try" in code.split("def batch_process")[1] if "def batch_process" in code else False))

    result = subprocess.run(
        ["python3", "-m", "pytest", "test_processor.py", "-v"],
        cwd=str(workdir), capture_output=True, text=True, timeout=30,
    )
    checks.append(("all tests pass", result.returncode == 0))

    return checks


def setup_t5_config_precedence(workdir: Path):
    """Tier 5, Test 36: Config precedence fix."""
    (workdir / "config_loader.py").write_text('''\
"""Configuration loader with precedence: defaults < file < env < CLI."""
import json
import os

DEFAULTS = {
    "host": "localhost",
    "port": 8080,
    "debug": False,
    "log_level": "INFO",
    "max_connections": 100,
}

def load_config(config_path: str = None, cli_overrides: dict = None) -> dict:
    """Load configuration with proper precedence.

    Priority (highest to lowest):
    1. CLI overrides
    2. Environment variables (APP_HOST, APP_PORT, etc.)
    3. Config file (JSON)
    4. Defaults

    BUG: Current implementation has wrong precedence —
    env vars override CLI, and file overrides env.
    """
    config = dict(DEFAULTS)

    # BUG: env is checked first, then file overwrites it
    for key in DEFAULTS:
        env_key = f"APP_{key.upper()}"
        if env_key in os.environ:
            val = os.environ[env_key]
            # Type coerce
            if isinstance(DEFAULTS[key], bool):
                config[key] = val.lower() in ("true", "1", "yes")
            elif isinstance(DEFAULTS[key], int):
                config[key] = int(val)
            else:
                config[key] = val

    # BUG: file overwrites env (should be: env > file)
    if config_path and os.path.exists(config_path):
        with open(config_path) as f:
            file_config = json.load(f)
            config.update(file_config)

    # BUG: CLI is applied but then env can still override via reload
    if cli_overrides:
        config.update(cli_overrides)

    return config
''')
    (workdir / "test_config.py").write_text('''\
import json
import os
import tempfile
from config_loader import load_config

def test_defaults():
    config = load_config()
    assert config["host"] == "localhost"
    assert config["port"] == 8080

def test_file_overrides_defaults():
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
        json.dump({"host": "filehost", "port": 9090}, f)
        path = f.name
    config = load_config(config_path=path)
    os.unlink(path)
    assert config["host"] == "filehost"
    assert config["port"] == 9090

def test_env_overrides_file():
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
        json.dump({"host": "filehost"}, f)
        path = f.name
    os.environ["APP_HOST"] = "envhost"
    try:
        config = load_config(config_path=path)
        assert config["host"] == "envhost", f"Expected envhost, got {config[\'host\']}"
    finally:
        del os.environ["APP_HOST"]
        os.unlink(path)

def test_cli_overrides_env():
    os.environ["APP_PORT"] = "7777"
    try:
        config = load_config(cli_overrides={"port": 5555})
        assert config["port"] == 5555, f"Expected 5555, got {config[\'port\']}"
    finally:
        del os.environ["APP_PORT"]

def test_full_precedence():
    """CLI > env > file > defaults."""
    with tempfile.NamedTemporaryFile(mode='w', suffix='.json', delete=False) as f:
        json.dump({"host": "filehost", "port": 9090, "debug": True}, f)
        path = f.name
    os.environ["APP_PORT"] = "7777"
    os.environ["APP_DEBUG"] = "false"
    try:
        config = load_config(config_path=path, cli_overrides={"port": 5555})
        assert config["host"] == "filehost"      # file wins (no CLI or env for host)
        assert config["port"] == 5555             # CLI wins over env and file
        assert config["debug"] == False           # env wins over file
        assert config["log_level"] == "INFO"      # default (nothing else sets it)
    finally:
        del os.environ["APP_PORT"]
        del os.environ["APP_DEBUG"]
        os.unlink(path)
''')
    return {
        "prompt": (
            "The config_loader.py has wrong precedence. The correct order should be:\n"
            "  defaults < config file < environment variables < CLI overrides\n"
            "Currently env vars are checked before the file (so file overwrites env), "
            "which is wrong. Fix the load_config function so precedence is correct. "
            "Run the tests to verify."
        ),
        "max_turns": 10,
    }


def check_t5_config_precedence(workdir: Path) -> list[tuple[str, bool]]:
    checks = []
    result = subprocess.run(
        ["python3", "-m", "pytest", "test_config.py", "-v"],
        cwd=str(workdir), capture_output=True, text=True, timeout=30,
    )
    checks.append(("all tests pass", result.returncode == 0))

    # Verify precedence logic by inspection
    code = (workdir / "config_loader.py").read_text()
    # File should be loaded before env vars in the code
    file_pos = code.find("config_path") if "config_path" in code.split("def load_config")[1] else 0
    checks.append(("function still works", "def load_config" in code))

    return checks


SCENARIOS = [
    ("T1.1: File Creation", "Tier 1", setup_t1_file_creation, check_t1_file_creation),
    ("T1.2: Bug Fix", "Tier 1", setup_t1_bug_fix, check_t1_bug_fix),
    ("T1.5: Test Generation", "Tier 1", setup_t1_test_generation, check_t1_test_generation),
    ("T2.9: Hidden Edge Cases", "Tier 2", setup_t2_hidden_edge_cases, check_t2_hidden_edge_cases),
    ("T2.14: Security Hardening", "Tier 2", setup_t2_security_hardening, check_t2_security_hardening),
    ("T3.18: Minimal Diff Fix", "Tier 3", setup_t3_minimal_diff, check_t3_minimal_diff),
    ("T3.28: State Leakage Fix", "Tier 3", setup_t3_state_leakage, check_t3_state_leakage),
    ("T4.27: Error Handling", "Tier 4", setup_t4_error_handling, check_t4_error_handling),
    ("T5.36: Config Precedence", "Tier 5", setup_t5_config_precedence, check_t5_config_precedence),
    ("TT.2: CSV → JSON", "Terminal", setup_terminal_csv_json, check_terminal_csv_json),
    ("TT.3: Git Archaeology", "Terminal", setup_terminal_git_archaeology, check_terminal_git_archaeology),
]


def run_pipit(workdir: Path, prompt: str, max_turns: int) -> tuple[int, float]:
    """Run pipit and return (return_code, elapsed_seconds)."""
    cmd = [
        PIPIT_BINARY, prompt,
        "--provider", PROVIDER,
        "--model", MODEL,
        "--base-url", BASE_URL,
        "--api-key", API_KEY,
        "--approval", "full_auto",
        "--max-turns", str(max_turns),
        "--root", str(workdir),
    ]

    start = time.time()
    try:
        result = subprocess.run(
            cmd, cwd=str(workdir), timeout=TIMEOUT,
            capture_output=True, text=True,
        )
        elapsed = time.time() - start
        if result.stderr:
            # Extract turn count from stderr
            for line in result.stderr.split("\n"):
                if "turns" in line:
                    pass  # Could parse turn count
        return result.returncode, elapsed
    except subprocess.TimeoutExpired:
        return -1, TIMEOUT


def run_scenario(name: str, tier: str, setup_fn, check_fn) -> BenchResult:
    """Run a single benchmark scenario."""
    workdir = Path(tempfile.mkdtemp(prefix=f"pipit-bench-{name.replace(' ', '_')}-"))
    print(f"\n{'='*60}")
    print(f"  {name} ({tier})")
    print(f"  Workdir: {workdir}")
    print(f"{'='*60}")

    try:
        # Setup
        config = setup_fn(workdir)
        prompt = config["prompt"]
        max_turns = config.get("max_turns", MAX_TURNS)

        # Run pipit
        print(f"  Running pipit (max {max_turns} turns)...")
        rc, elapsed = run_pipit(workdir, prompt, max_turns)
        print(f"  Finished in {elapsed:.1f}s (rc={rc})")

        # Check results
        checks = check_fn(workdir)
        passed = sum(1 for _, ok in checks if ok)
        total = len(checks)

        print(f"  Checks: {passed}/{total}")
        for check_name, ok in checks:
            status = "✓" if ok else "✗"
            print(f"    {status} {check_name}")

        return BenchResult(
            name=name, tier=tier,
            passed=(passed == total),
            checks_passed=passed, checks_total=total,
            turns=0, elapsed=elapsed,
            details="\n".join(f"  {'✓' if ok else '✗'} {n}" for n, ok in checks),
        )

    except Exception as e:
        return BenchResult(
            name=name, tier=tier,
            passed=False, checks_passed=0, checks_total=0,
            turns=0, elapsed=0, error=str(e),
        )
    finally:
        # Cleanup
        shutil.rmtree(workdir, ignore_errors=True)


def main():
    print("=" * 60)
    print("  Pipit CLI Agent — Real E2E Benchmark Runner")
    print(f"  Model: {MODEL}")
    print(f"  Endpoint: {BASE_URL}")
    print(f"  Binary: {PIPIT_BINARY}")
    print("=" * 60)

    # Verify endpoint
    try:
        import urllib.request
        r = urllib.request.urlopen(f"{BASE_URL}/v1/models", timeout=5)
        print(f"  LLM endpoint: OK")
    except Exception as e:
        print(f"  LLM endpoint: FAILED ({e})")
        sys.exit(1)

    # Verify pipit binary
    if not Path(PIPIT_BINARY).exists():
        print(f"  Binary not found: {PIPIT_BINARY}")
        sys.exit(1)
    print(f"  Binary: OK")

    # Run scenarios
    results = []
    for name, tier, setup_fn, check_fn in SCENARIOS:
        result = run_scenario(name, tier, setup_fn, check_fn)
        results.append(result)

    # Summary
    print("\n" + "=" * 60)
    print("  BENCHMARK SUMMARY")
    print("=" * 60)
    print(f"\n{'Test':<35} {'Tier':<10} {'Checks':<10} {'Time':<8} {'Result':<8}")
    print("-" * 71)

    total_pass = 0
    total_tests = 0
    total_checks = 0
    total_checks_passed = 0

    for r in results:
        status = "PASS" if r.passed else "FAIL"
        checks = f"{r.checks_passed}/{r.checks_total}"
        time_str = f"{r.elapsed:.0f}s"
        print(f"{r.name:<35} {r.tier:<10} {checks:<10} {time_str:<8} {status:<8}")
        total_tests += 1
        if r.passed:
            total_pass += 1
        total_checks += r.checks_total
        total_checks_passed += r.checks_passed

    print("-" * 71)
    print(f"{'TOTAL':<35} {'':10} {total_checks_passed}/{total_checks:<10} {'':8} {total_pass}/{total_tests}")
    print(f"\nPass rate: {total_pass}/{total_tests} ({100*total_pass/total_tests:.0f}%)")
    print(f"Check rate: {total_checks_passed}/{total_checks} ({100*total_checks_passed/total_checks:.0f}%)")

    # Write results JSON
    results_path = Path(__file__).parent.parent / "benchmark_results.json"
    results_json = [{
        "name": r.name, "tier": r.tier, "passed": r.passed,
        "checks_passed": r.checks_passed, "checks_total": r.checks_total,
        "elapsed": round(r.elapsed, 1), "details": r.details, "error": r.error,
    } for r in results]
    results_path.write_text(json.dumps(results_json, indent=2))
    print(f"\nResults written to {results_path}")


if __name__ == "__main__":
    main()
