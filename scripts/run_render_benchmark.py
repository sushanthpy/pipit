#!/usr/bin/env python3
"""
Render Blog AI Coding Agents Benchmark — Pipit Edition.

Replicates the benchmark from https://render.com/blog/ai-coding-agents-benchmark
using a real LLM call to a local vLLM endpoint.

The benchmark:
  1. Sends the EXACT prompt from the Render blog asking an LLM to build
     a URL shortener in Next.js + MUI + PostgreSQL + Dockerfile.
  2. Uses pipit CLI to execute the task agenically (tool use, file creation, etc.)
  3. Validates the output across 7 categories matching the blog's scoring.
  4. Produces a scored report.

Usage:
    python3 scripts/run_render_benchmark.py [--pipit-only | --llm-only | --full]

Modes:
    --pipit-only   Run pipit agent against the prompt (default)
    --llm-only     Single-shot LLM completion (no tool use)
    --full         Both modes
"""

import argparse
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
PIPIT_BINARY = os.environ.get(
    "PIPIT_BIN",
    str(Path(__file__).parent.parent / "target" / "debug" / "pipit"),
)
PROVIDER = "openai"
MODEL = "Qwen/Qwen3.5-35B-A3B-FP8"
BASE_URL = "http://192.168.1.198:8000"
API_KEY = "dummy"
MAX_TURNS = 30  # Generous — vibe coding takes many steps
TIMEOUT = 600   # 10 minutes for a full app build

# ── The exact prompt from the Render blog ──
RENDER_BLOG_PROMPT = """\
Please build a simple url shortener app. Please build it in nextjs with \
a minimalist style using the mui component library. The app should have \
a single input field that takes in a URL from the user and returns a \
shortened/encoded url. For the backend, provide a postgres connection \
for connecting to a database and storing the shortened urls. The app \
should be deployable via a dockerfile."""

# ── Scoring categories from the blog ──
CATEGORIES = [
    "app_completeness",
    "code_quality",
    "ui_styling",
    "database_setup",
    "docker_setup",
    "error_handling",
    "project_structure",
]


@dataclass
class CategoryScore:
    name: str
    score: int  # 0-10
    max_score: int = 10
    checks: list = field(default_factory=list)
    notes: str = ""


@dataclass
class BenchmarkResult:
    mode: str
    elapsed: float
    categories: list  # list of CategoryScore
    total_score: int = 0
    max_score: int = 0
    follow_up_prompts: int = 0
    files_created: list = field(default_factory=list)
    error: str = ""


# ═══════════════════════════════════════════════════════════════════════════
#  Pipit Agent Mode
# ═══════════════════════════════════════════════════════════════════════════

def run_pipit_benchmark(workdir: Path) -> BenchmarkResult:
    """Run pipit agent against the Render blog prompt."""
    print(f"\n{'='*70}")
    print(f"  RENDER BLOG BENCHMARK — Pipit Agent Mode")
    print(f"  Model:   {MODEL}")
    print(f"  Endpoint: {BASE_URL}")
    print(f"  Workdir: {workdir}")
    print(f"{'='*70}\n")

    # Initialize as a node project directory
    (workdir / ".gitkeep").write_text("")

    cmd = [
        PIPIT_BINARY, RENDER_BLOG_PROMPT,
        "--provider", PROVIDER,
        "--model", MODEL,
        "--base-url", BASE_URL,
        "--api-key", API_KEY,
        "--approval", "full_auto",
        "--max-turns", str(MAX_TURNS),
        "--root", str(workdir),
    ]

    print(f"  Running pipit (max {MAX_TURNS} turns, timeout {TIMEOUT}s)...")
    start = time.time()

    try:
        result = subprocess.run(
            cmd, cwd=str(workdir), timeout=TIMEOUT,
            capture_output=True, text=True,
        )
        elapsed = time.time() - start
        print(f"  Finished in {elapsed:.1f}s (rc={result.returncode})")

        if result.stdout:
            # Save raw output
            (workdir / "_pipit_stdout.txt").write_text(result.stdout)
        if result.stderr:
            (workdir / "_pipit_stderr.txt").write_text(result.stderr)

    except subprocess.TimeoutExpired:
        elapsed = TIMEOUT
        print(f"  TIMED OUT after {TIMEOUT}s")
        return BenchmarkResult(
            mode="pipit",
            elapsed=elapsed,
            categories=[],
            error="Timed out",
        )

    # Collect created files
    files = []
    for f in workdir.rglob("*"):
        if f.is_file() and not f.name.startswith("_pipit") and ".git" not in str(f):
            rel = f.relative_to(workdir)
            files.append(str(rel))
    files.sort()

    print(f"\n  Files created ({len(files)}):")
    for f in files[:30]:
        print(f"    {f}")
    if len(files) > 30:
        print(f"    ... and {len(files) - 30} more")

    # Run checks
    categories = score_output(workdir)

    total = sum(c.score for c in categories)
    max_total = sum(c.max_score for c in categories)

    return BenchmarkResult(
        mode="pipit",
        elapsed=elapsed,
        categories=categories,
        total_score=total,
        max_score=max_total,
        files_created=files,
    )


# ═══════════════════════════════════════════════════════════════════════════
#  Single-Shot LLM Mode (no tools, just completion)
# ═══════════════════════════════════════════════════════════════════════════

def run_llm_benchmark(workdir: Path) -> BenchmarkResult:
    """Single-shot LLM completion (code generation only, no tool use)."""
    print(f"\n{'='*70}")
    print(f"  RENDER BLOG BENCHMARK — Single-Shot LLM Mode")
    print(f"  Model:   {MODEL}")
    print(f"  Endpoint: {BASE_URL}")
    print(f"{'='*70}\n")

    try:
        import urllib.request

        system_prompt = (
            "You are an expert full-stack developer. When asked to build an app, "
            "provide the COMPLETE implementation with all files needed. "
            "For each file, use this format:\n\n"
            "### FILE: path/to/file.ext\n```\nfile contents\n```\n\n"
            "Include ALL files: package.json, all source files, Dockerfile, etc."
        )

        payload = {
            "model": MODEL,
            "messages": [
                {"role": "system", "content": system_prompt},
                {"role": "user", "content": RENDER_BLOG_PROMPT},
            ],
            "max_tokens": 16384,
            "temperature": 0.7,
        }

        req = urllib.request.Request(
            f"{BASE_URL}/v1/chat/completions",
            data=json.dumps(payload).encode(),
            headers={"Content-Type": "application/json"},
        )

        print("  Sending LLM request...")
        start = time.time()
        with urllib.request.urlopen(req, timeout=300) as resp:
            data = json.loads(resp.read())
        elapsed = time.time() - start
        print(f"  Response received in {elapsed:.1f}s")

        content = data["choices"][0]["message"]["content"]
        (workdir / "_llm_response.md").write_text(content)

        # Parse files from response
        files_written = extract_and_write_files(content, workdir)
        print(f"\n  Files extracted ({len(files_written)}):")
        for f in files_written:
            print(f"    {f}")

        # Score
        categories = score_output(workdir)
        total = sum(c.score for c in categories)
        max_total = sum(c.max_score for c in categories)

        return BenchmarkResult(
            mode="llm-single-shot",
            elapsed=elapsed,
            categories=categories,
            total_score=total,
            max_score=max_total,
            files_created=files_written,
        )

    except Exception as e:
        import traceback
        traceback.print_exc()
        return BenchmarkResult(
            mode="llm-single-shot",
            elapsed=0,
            categories=[],
            error=str(e),
        )


def extract_and_write_files(content: str, workdir: Path) -> list[str]:
    """Extract files from LLM response in ### FILE: path format."""
    import re

    files = []
    # Pattern: ### FILE: path/to/file\n```lang?\ncontents\n```
    pattern = r'###\s+FILE:\s*([^\n]+)\n```[^\n]*\n(.*?)```'
    matches = re.findall(pattern, content, re.DOTALL)

    for filepath, file_content in matches:
        filepath = filepath.strip()
        # Security: prevent path traversal
        if ".." in filepath:
            continue
        target = workdir / filepath
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(file_content)
        files.append(filepath)

    return files


# ═══════════════════════════════════════════════════════════════════════════
#  Scoring Engine — matches the Render blog's 7 categories
# ═══════════════════════════════════════════════════════════════════════════

def score_output(workdir: Path) -> list[CategoryScore]:
    """Score the generated output across all categories."""
    return [
        check_app_completeness(workdir),
        check_code_quality(workdir),
        check_ui_styling(workdir),
        check_database_setup(workdir),
        check_docker_setup(workdir),
        check_error_handling(workdir),
        check_project_structure(workdir),
    ]


def _find_files(workdir: Path, patterns: list[str]) -> list[Path]:
    """Find files matching any of the given glob patterns."""
    results = []
    for pat in patterns:
        for p in workdir.rglob(pat):
            if p.is_file() and "node_modules" not in str(p) and ".next" not in str(p):
                results.append(p)
    return results


def _read_all_source(workdir: Path) -> str:
    """Read all source files into one string for searching."""
    content = ""
    for ext in ["*.js", "*.jsx", "*.ts", "*.tsx", "*.json", "*.py", "*.sql"]:
        for f in workdir.rglob(ext):
            if "node_modules" not in str(f) and ".next" not in str(f):
                try:
                    content += f"\n--- {f.relative_to(workdir)} ---\n"
                    content += f.read_text(errors="ignore")
                except Exception:
                    pass
    return content


def check_app_completeness(workdir: Path) -> CategoryScore:
    """Is this a complete, functional URL shortener?"""
    checks = []
    all_src = _read_all_source(workdir)

    # 1. Has package.json
    pkg_files = _find_files(workdir, ["package.json"])
    has_pkg = len(pkg_files) > 0
    checks.append(("package.json exists", has_pkg))

    # 2. Next.js dependency
    has_next = "next" in all_src.lower() and has_pkg
    checks.append(("Next.js dependency", has_next))

    # 3. URL input field
    has_input = any(kw in all_src.lower() for kw in ["<input", "<textfield", "textfield", "input", "url"])
    checks.append(("URL input field", has_input))

    # 4. URL shortening logic (encoding/hashing)
    has_shorten = any(kw in all_src.lower() for kw in [
        "shorten", "encode", "hash", "nanoid", "shortid", "base62",
        "base64", "crypto", "randomstring", "random", "uuid",
    ])
    checks.append(("URL shortening logic", has_shorten))

    # 5. API route for shortening
    has_api = bool(_find_files(workdir, ["**/api/**/*.ts", "**/api/**/*.js", "**/api/**/*.tsx", "**/api/**/*.jsx"]))
    if not has_api:
        has_api = "api/" in all_src or "fetch(" in all_src
    checks.append(("API route exists", has_api))

    # 6. Redirect logic (looking up and redirecting to original URL)
    has_redirect = any(kw in all_src.lower() for kw in [
        "redirect", "302", "307", "location", "router.push",
    ])
    checks.append(("Redirect logic", has_redirect))

    passed = sum(1 for _, ok in checks if ok)
    score = min(10, int(passed / len(checks) * 10))

    return CategoryScore(
        name="App Completeness",
        score=score,
        checks=checks,
        notes=f"{passed}/{len(checks)} completeness checks passed",
    )


def check_code_quality(workdir: Path) -> CategoryScore:
    """Code quality: types, imports, structure."""
    checks = []
    all_src = _read_all_source(workdir)

    # TypeScript used
    ts_files = _find_files(workdir, ["*.ts", "*.tsx"])
    ts_files = [f for f in ts_files if "node_modules" not in str(f)]
    checks.append(("Uses TypeScript", len(ts_files) > 0))

    # Type annotations
    has_types = any(kw in all_src for kw in [": string", ": number", ": boolean", "interface ", "type "])
    checks.append(("Type annotations", has_types))

    # Error handling in code
    has_try = "try" in all_src and "catch" in all_src
    checks.append(("Try/catch error handling", has_try))

    # Async/await
    has_async = "async" in all_src and "await" in all_src
    checks.append(("Async/await usage", has_async))

    # No obvious security issues (no eval, no raw SQL concat)
    no_eval = "eval(" not in all_src
    checks.append(("No eval()", no_eval))

    # Parameterized queries (not string concat)
    if "query" in all_src.lower() or "sql" in all_src.lower():
        has_params = any(kw in all_src for kw in ["$1", "$2", "?", "parameterized", "values("])
        checks.append(("Parameterized SQL queries", has_params))

    passed = sum(1 for _, ok in checks if ok)
    score = min(10, int(passed / len(checks) * 10))

    return CategoryScore(
        name="Code Quality",
        score=score,
        checks=checks,
        notes=f"{passed}/{len(checks)} quality checks passed",
    )


def check_ui_styling(workdir: Path) -> CategoryScore:
    """MUI component library usage and styling."""
    checks = []
    all_src = _read_all_source(workdir)

    # MUI dependency
    has_mui = any(kw in all_src for kw in [
        "@mui/material", "@mui/icons", "material-ui",
        "from '@mui", 'from "@mui',
    ])
    checks.append(("MUI dependency", has_mui))

    # MUI components used
    mui_components = ["Button", "TextField", "Container", "Typography", "Box", "Paper", "AppBar"]
    used = sum(1 for c in mui_components if c in all_src)
    checks.append((f"MUI components used ({used})", used >= 2))

    # ThemeProvider or custom theme
    has_theme = any(kw in all_src for kw in ["ThemeProvider", "createTheme", "theme"])
    checks.append(("Theme configuration", has_theme))

    # Responsive design (media queries or MUI responsive)
    has_responsive = any(kw in all_src.lower() for kw in [
        "@media", "usemediequery", "breakpoint", "sx=", "grid", "stack",
    ])
    checks.append(("Responsive design", has_responsive))

    # Success/error message display
    has_messages = any(kw in all_src for kw in [
        "Snackbar", "Alert", "success", "error", "message", "toast",
    ])
    checks.append(("Success/error messages", has_messages))

    passed = sum(1 for _, ok in checks if ok)
    score = min(10, int(passed / len(checks) * 10))

    return CategoryScore(
        name="UI / Styling",
        score=score,
        checks=checks,
        notes=f"{passed}/{len(checks)} UI checks passed",
    )


def check_database_setup(workdir: Path) -> CategoryScore:
    """PostgreSQL connection and schema."""
    checks = []
    all_src = _read_all_source(workdir)

    # PostgreSQL mentioned / pg dependency
    has_pg = any(kw in all_src.lower() for kw in [
        "postgres", "pg", "postgresql", "prisma", "@prisma", "pool", "knex",
        "sequelize", "typeorm", "drizzle",
    ])
    checks.append(("PostgreSQL dependency/mention", has_pg))

    # Connection string / pool
    has_conn = any(kw in all_src for kw in [
        "DATABASE_URL", "connectionString", "Pool", "pool", "createPool",
        "PrismaClient", "new Pool", "pg.Pool",
    ])
    checks.append(("DB connection setup", has_conn))

    # Table creation / Schema
    has_schema = any(kw in all_src.lower() for kw in [
        "create table", "createtable", "migration", "schema", "prisma",
        "urls", "short_url", "original_url",
    ])
    checks.append(("DB schema/table definition", has_schema))

    # SQL migration or init script
    sql_files = _find_files(workdir, ["*.sql", "**/migrations/**"])
    has_migration = len(sql_files) > 0 or "CREATE TABLE" in all_src.upper()
    checks.append(("SQL migration/init", has_migration))

    # Docker Compose with DB
    compose_files = _find_files(workdir, ["docker-compose*", "compose*"])
    has_compose_db = False
    for cf in compose_files:
        content = cf.read_text(errors="ignore")
        if "postgres" in content.lower():
            has_compose_db = True
    checks.append(("Docker Compose with PostgreSQL", has_compose_db))

    passed = sum(1 for _, ok in checks if ok)
    score = min(10, int(passed / len(checks) * 10))

    return CategoryScore(
        name="Database Setup",
        score=score,
        checks=checks,
        notes=f"{passed}/{len(checks)} DB checks passed",
    )


def check_docker_setup(workdir: Path) -> CategoryScore:
    """Dockerfile and containerization."""
    checks = []
    all_src = _read_all_source(workdir)

    # Dockerfile exists
    dockerfiles = _find_files(workdir, ["Dockerfile", "dockerfile"])
    has_dockerfile = len(dockerfiles) > 0
    checks.append(("Dockerfile exists", has_dockerfile))

    docker_content = ""
    if has_dockerfile:
        docker_content = dockerfiles[0].read_text(errors="ignore")

    # Multi-stage build
    has_multistage = docker_content.lower().count("from ") >= 2
    checks.append(("Multi-stage build", has_multistage))

    # Node base image
    has_node = "node:" in docker_content.lower() or "node" in docker_content.lower()
    checks.append(("Node.js base image", has_node))

    # COPY and npm install
    has_install = "npm" in docker_content or "yarn" in docker_content or "pnpm" in docker_content
    checks.append(("Package install step", has_install))

    # EXPOSE port
    has_expose = "EXPOSE" in docker_content or "expose" in docker_content
    checks.append(("EXPOSE port directive", has_expose))

    # Docker Compose
    compose_files = _find_files(workdir, ["docker-compose*", "compose*"])
    has_compose = len(compose_files) > 0
    checks.append(("Docker Compose file", has_compose))

    # .dockerignore (may be in a subdirectory)
    has_ignore = bool(_find_files(workdir, [".dockerignore"]))
    checks.append((".dockerignore file", has_ignore))

    passed = sum(1 for _, ok in checks if ok)
    score = min(10, int(passed / len(checks) * 10))

    return CategoryScore(
        name="Docker Setup",
        score=score,
        checks=checks,
        notes=f"{passed}/{len(checks)} Docker checks passed",
    )


def check_error_handling(workdir: Path) -> CategoryScore:
    """Error handling and edge cases."""
    checks = []
    all_src = _read_all_source(workdir)

    # Input validation
    has_validation = any(kw in all_src.lower() for kw in [
        "valid", "url.parse", "new url", "isvalid", "regex", "pattern",
        "http://", "https://",
    ])
    checks.append(("URL validation", has_validation))

    # Error responses (HTTP status codes)
    has_status = any(kw in all_src for kw in ["400", "404", "500", "status("])
    checks.append(("HTTP status codes", has_status))

    # User-facing error messages
    has_user_errors = any(kw in all_src.lower() for kw in [
        "invalid url", "error", "please enter", "invalid input",
    ])
    checks.append(("User-facing error messages", has_user_errors))

    # Loading state
    has_loading = any(kw in all_src.lower() for kw in [
        "loading", "isloading", "spinner", "circularProgress",
    ])
    checks.append(("Loading state", has_loading))

    passed = sum(1 for _, ok in checks if ok)
    score = min(10, int(passed / len(checks) * 10))

    return CategoryScore(
        name="Error Handling",
        score=score,
        checks=checks,
        notes=f"{passed}/{len(checks)} error handling checks passed",
    )


def check_project_structure(workdir: Path) -> CategoryScore:
    """Overall project structure quality."""
    checks = []

    # Detect project root — pipit may create the app in a subdirectory
    project_root = workdir
    for child in workdir.iterdir():
        if child.is_dir() and (child / "package.json").exists():
            project_root = child
            break

    # Has README
    has_readme = any(
        (d / name).exists()
        for d in (workdir, project_root)
        for name in ("README.md", "readme.md", "README")
    )
    checks.append(("README.md", has_readme))

    # Has .env.example or .env
    has_env = any(
        (d / name).exists()
        for d in (workdir, project_root)
        for name in (".env.example", ".env.local.example", ".env")
    )
    checks.append(("Environment config (.env)", has_env))

    # Organized directory structure (src/ or app/ or pages/)
    has_src = any(
        (d / sub).exists()
        for d in (workdir, project_root)
        for sub in ("src", "app", "pages")
    )
    checks.append(("Organized source directory", has_src))

    # package.json with scripts
    pkg_path = project_root / "package.json"
    if pkg_path.exists():
        try:
            pkg = json.loads(pkg_path.read_text())
            has_scripts = "scripts" in pkg and "dev" in pkg.get("scripts", {})
            checks.append(("Dev script in package.json", has_scripts))
        except Exception:
            checks.append(("Dev script in package.json", False))

    # tsconfig or jsconfig
    has_config = any(
        (d / name).exists()
        for d in (workdir, project_root)
        for name in ("tsconfig.json", "jsconfig.json")
    )
    checks.append(("TS/JS config", has_config))

    passed = sum(1 for _, ok in checks if ok)
    score = min(10, int(passed / len(checks) * 10))

    return CategoryScore(
        name="Project Structure",
        score=score,
        checks=checks,
        notes=f"{passed}/{len(checks)} structure checks passed",
    )


# ═══════════════════════════════════════════════════════════════════════════
#  Report Generation
# ═══════════════════════════════════════════════════════════════════════════

def print_report(result: BenchmarkResult):
    """Print a formatted benchmark report."""
    print(f"\n{'='*70}")
    print(f"  RENDER BLOG BENCHMARK — RESULTS")
    print(f"  Mode: {result.mode}")
    print(f"  Time: {result.elapsed:.1f}s")
    print(f"{'='*70}\n")

    if result.error:
        print(f"  ❌ ERROR: {result.error}\n")
        return

    print(f"  {'Category':<25} {'Score':<8} {'Details':<40}")
    print(f"  {'-'*73}")

    for cat in result.categories:
        bar = "█" * cat.score + "░" * (cat.max_score - cat.score)
        print(f"  {cat.name:<25} {cat.score:>2}/{cat.max_score}    {bar}  {cat.notes}")
        for check_name, ok in cat.checks:
            status = "✓" if ok else "✗"
            print(f"    {status} {check_name}")

    print(f"\n  {'─'*73}")
    print(f"  {'TOTAL':<25} {result.total_score:>2}/{result.max_score}    "
          f"{'█' * result.total_score}{'░' * (result.max_score - result.total_score)}")
    pct = result.total_score / result.max_score * 100 if result.max_score else 0
    print(f"  Percentage: {pct:.0f}%")

    # Render blog equivalent score (out of 10)
    render_score = round(result.total_score / result.max_score * 10, 1)
    print(f"  Render Blog Equivalent: {render_score}/10\n")


def save_results(result: BenchmarkResult, output_path: Path):
    """Save results to JSON."""
    data = {
        "benchmark": "Render Blog AI Coding Agents Benchmark",
        "source": "https://render.com/blog/ai-coding-agents-benchmark",
        "model": MODEL,
        "endpoint": BASE_URL,
        "mode": result.mode,
        "elapsed_seconds": round(result.elapsed, 1),
        "total_score": result.total_score,
        "max_score": result.max_score,
        "percentage": round(result.total_score / result.max_score * 100, 1) if result.max_score else 0,
        "render_equivalent": round(result.total_score / result.max_score * 10, 1) if result.max_score else 0,
        "categories": [
            {
                "name": c.name,
                "score": c.score,
                "max_score": c.max_score,
                "checks": [{"name": n, "passed": ok} for n, ok in c.checks],
                "notes": c.notes,
            }
            for c in result.categories
        ],
        "files_created": result.files_created,
        "error": result.error,
    }
    output_path.write_text(json.dumps(data, indent=2))
    print(f"  Results saved to: {output_path}")


# ═══════════════════════════════════════════════════════════════════════════
#  Main
# ═══════════════════════════════════════════════════════════════════════════

def main():
    parser = argparse.ArgumentParser(description="Render Blog AI Coding Agents Benchmark")
    parser.add_argument("--pipit-only", action="store_true", default=True, help="Run pipit agent mode only")
    parser.add_argument("--llm-only", action="store_true", help="Run single-shot LLM mode only")
    parser.add_argument("--full", action="store_true", help="Run both modes")
    parser.add_argument("--keep-workdir", action="store_true", help="Don't delete work directory")
    parser.add_argument("-o", "--output", default=None, help="Output directory for results")
    args = parser.parse_args()

    if args.llm_only:
        args.pipit_only = False

    # Verify endpoint
    try:
        import urllib.request
        r = urllib.request.urlopen(f"{BASE_URL}/v1/models", timeout=5)
        models = json.loads(r.read())
        print(f"  ✓ LLM endpoint OK: {[m['id'] for m in models.get('data', [])]}")
    except Exception as e:
        print(f"  ✗ LLM endpoint FAILED: {e}")
        sys.exit(1)

    output_dir = Path(args.output) if args.output else Path(__file__).parent.parent / "results" / "render-benchmark"
    output_dir.mkdir(parents=True, exist_ok=True)

    results = []

    # ── Pipit Agent Mode ──
    if args.pipit_only or args.full:
        workdir = Path(tempfile.mkdtemp(prefix="render-bench-pipit-"))
        try:
            result = run_pipit_benchmark(workdir)
            print_report(result)
            save_results(result, output_dir / "pipit_results.json")
            results.append(result)
        finally:
            if not args.keep_workdir:
                shutil.rmtree(workdir, ignore_errors=True)
            else:
                print(f"  Workdir preserved: {workdir}")

    # ── Single-Shot LLM Mode ──
    if args.llm_only or args.full:
        workdir = Path(tempfile.mkdtemp(prefix="render-bench-llm-"))
        try:
            result = run_llm_benchmark(workdir)
            print_report(result)
            save_results(result, output_dir / "llm_results.json")
            results.append(result)
        finally:
            if not args.keep_workdir:
                shutil.rmtree(workdir, ignore_errors=True)
            else:
                print(f"  Workdir preserved: {workdir}")

    # ── Comparison (if both) ──
    if len(results) == 2 and not any(r.error for r in results):
        print(f"\n{'='*70}")
        print(f"  COMPARISON")
        print(f"{'='*70}\n")
        print(f"  {'Category':<25} {'Pipit':<10} {'LLM':<10}")
        print(f"  {'-'*45}")
        for i, cat_name in enumerate(CATEGORIES):
            p_score = results[0].categories[i].score if i < len(results[0].categories) else 0
            l_score = results[1].categories[i].score if i < len(results[1].categories) else 0
            leader = "←" if p_score > l_score else ("→" if l_score > p_score else "=")
            print(f"  {cat_name:<25} {p_score:>2}/10      {l_score:>2}/10     {leader}")
        print(f"\n  {'TOTAL':<25} {results[0].total_score:>2}/{results[0].max_score}      {results[1].total_score:>2}/{results[1].max_score}")


if __name__ == "__main__":
    main()
