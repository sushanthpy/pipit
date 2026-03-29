<p align="center">
  <img src="main-pipit.png" alt="Pipit — AI coding agent for the terminal" width="700">
</p>

<p align="center">
  <img src="full-img.png" alt="Pipit — AI coding agent for the terminal" width="700">
</p>

```
        _._
       (o >
      / / \
     (_|  /
       " "
```

# Pipit

**Pipit is an AI coding agent for the terminal.**
It reads code, edits files, runs shell commands, and works through coding tasks with an LLM — without leaving your terminal.

Built for real codebases, not toy prompts.

---

## Quick start

```sh
# Install
curl -fsSL https://raw.githubusercontent.com/sushanthpy/pipit/main/install.sh | sh

# Configure (interactive — asks provider, model, API key, etc.)
pipit setup

# Start coding
pipit
```

That's it. `pipit setup` saves your config to `~/.config/pipit/config.toml` so you never have to pass flags again.

---

## Install

**One-line install:**

```sh
curl -fsSL https://raw.githubusercontent.com/sushanthpy/pipit/main/install.sh | sh
```

**Specific version:**

```sh
curl -fsSL https://raw.githubusercontent.com/sushanthpy/pipit/main/install.sh | sh -s v0.1.0
```

**Custom install directory:**

```sh
PIPIT_INSTALL_DIR=~/.local/bin curl -fsSL https://raw.githubusercontent.com/sushanthpy/pipit/main/install.sh | sh
```

**Build from source:**

```sh
git clone https://github.com/sushanthpy/pipit.git
cd pipit
cargo build --release
cp target/release/pipit /usr/local/bin/
```

---

## Setup

Run `pipit setup` to configure interactively:

```
$ pipit setup

  pipit setup
  Interactive configuration wizard

  Provider
  Supported: anthropic, openai, deepseek, google, openrouter,
             ollama, groq, cerebras, mistral, xai, openai_compatible

  Provider [anthropic]: openai
  Model [gpt-4o]: gpt-4o
  API Key: sk-...

  Approval Mode
    suggest     — read-only, ask before every change
    auto_edit   — auto-apply edits, ask for commands
    full_auto   — autonomous, no confirmation needed
  Approval mode [full_auto]: full_auto

  Max turns [25]: 25

  ✓ Config saved to ~/.config/pipit/config.toml
```

This generates a config file:

```toml
# ~/.config/pipit/config.toml
approval = "full_auto"

[provider]
default = "openai"

[model]
default_model = "gpt-4o"

[context]
max_turns = 25
```

You can also use a **local model** (Ollama, vLLM, etc.):

```
Provider: openai_compatible
Model: Qwen/Qwen3.5-35B-A3B-FP8
Base URL: http://localhost:8000
```

Or skip setup and pass flags directly:

```sh
pipit --provider anthropic --model claude-sonnet-4-20250514 --api-key sk-...
```

Or use environment variables:

```sh
export ANTHROPIC_API_KEY=sk-...
pipit
```

---

## Full-screen TUI

Pipit launches a full-screen terminal UI by default with a two-column layout:

```
┌─ status ───────────────────────────────────────────────────────────┐
│ pipit · repo · main · gpt-4o · Full access · 12% $0.0042          │
├─ task / phase ─────────────────────────────────────────────────────┤
│ task: fix the login bug            phase: executing                │
├─ timeline ──────────────┬─ response ───────────────────────────────┤
│ › fix the login bug     │ The issue is in `auth.rs` line 42.      │
│ ○ Read src/auth.rs      │ The session token check uses `==`       │
│ ● Edit src/auth.rs      │ instead of `eq()` for string compare.   │
│ ▸ $ cargo test          │                                          │
│ ✓ edit_file done        │ I've fixed it and the tests pass.       │
│ · turn 1 complete       │                                          │
├─ composer ─────────────────────────────────────────────────────────┤
│ you› _                                                             │
│ /help · @file · !shell · Esc cancel · Ctrl-C quit                 │
└────────────────────────────────────────────────────────────────────┘
```

- **Status bar** — repo, branch, model, approval mode, token usage, cost
- **Timeline** (left) — compact log of agent actions (reads, edits, shell commands)
- **Response** (right) — full model output and streaming text
- **Composer** (bottom) — type your prompt, see hints for commands

Use `--classic` for the old-style REPL if you prefer:

```sh
pipit --classic
```

---

## Commands

Type these in the composer:

| Command | Description |
|---------|-------------|
| `/help` | Show full help with examples |
| `/status` | Show repo, model, tokens, cost |
| `/cost` | Token cost summary |
| `/clear` | Reset context and chat history |
| `/quit` or `/q` | Exit pipit |
| `/plans` | Show proof-packet plan history |
| `/context` or `/ctx` | Show files in working set |
| `/tokens` | Token usage breakdown |
| `/compact` | Compress context to free tokens |
| `/add <file>` | Add file to working set |
| `/drop <file>` | Remove file from working set |
| `/plan [goal]` | Enter plan-first mode |
| `/verify [scope]` | Run build/lint/test checks |
| `/aside <question>` | Quick side question |
| `/bench <cmd>` | Run benchmarks (run/results/compare) |
| `/browse <url>` | Browser automation |
| `/mesh <cmd>` | Multi-agent mesh (status/delegate) |
| `/mcp <cmd>` | MCP server management (status/tools) |
| `/plugins <cmd>` | Plugin marketplace (list/install/uninstall) |
| `/deps <cmd>` | Dependency management (audit/outdated/tree) |
| `/watch <cmd>` | File watcher (start/stop/deps) |
| `/loop [n]` | Re-run last prompt N times |

### Grammar

```
/command           Slash commands (see above)
@file.rs           Attach file as context
!ls -la            Run shell command directly
↑ ↓                Scroll timeline
Esc                Cancel running agent
Ctrl-C             Quit
```

### Examples

```sh
# Ask about code
explain this codebase

# Attach a file and ask about it
@src/main.rs fix the panic on line 42

# Run a shell command through the agent
!cargo test -- --nocapture

# Add context and plan
/add src/lib.rs
/plan refactor the error handling
```

---

## Agent modes

Pipit supports four agent modes that control how much verification happens:

```sh
pipit --mode fast       # Default. Single executor loop, no verification.
pipit --mode balanced   # Verify only when the agent mutates files.
pipit --mode guarded    # Full Plan → Execute → Verify cycle with repair loops.
pipit --mode custom     # Guarded + role-specific model overrides.
```

| Mode | Planning | Verification | Repair | Use case |
|------|----------|-------------|--------|----------|
| `fast` | No | No | No | Quick questions, exploration |
| `balanced` | No | On mutation | No | Day-to-day editing |
| `guarded` | Yes | Always | Up to 2 | Critical changes, refactors |
| `custom` | Yes | Always | Up to 2 | Multi-model setups |

### Custom mode with different models

Use a fast model for execution and a strong model for planning/verification:

```sh
pipit --mode custom \
  --planner-model claude-sonnet-4-20250514 \
  --planner-provider anthropic \
  --verifier-model claude-sonnet-4-20250514 \
  --verifier-provider anthropic
```

---

## Approval modes

Control how much autonomy the agent has:

| Mode | What it does |
|------|-------------|
| `suggest` | Read-only. Ask before every change. |
| `auto_edit` | Auto-apply file edits, ask before shell commands. |
| `full_auto` | Fully autonomous, no confirmations. |

```sh
pipit --approval suggest     # Conservative
pipit --approval auto_edit   # Default
pipit --approval full_auto   # Full autonomy
```

---

## Configuration

Pipit resolves config from multiple layers (later wins):

1. Compiled defaults
2. `/etc/pipit/config.toml` (system-wide)
3. `~/.config/pipit/config.toml` (user — created by `pipit setup`)
4. `.pipit/config.toml` (project-level)
5. `PIPIT_*` environment variables
6. CLI flags (highest priority)

### Environment variables

```sh
export PIPIT_PROVIDER=openai
export PIPIT_MODEL=gpt-4o
export PIPIT_APPROVAL_MODE=full_auto
export PIPIT_MAX_TURNS=25
```

### Project-level config

Create `.pipit/config.toml` in your project root to set project-specific defaults:

```toml
# .pipit/config.toml
approval = "full_auto"

[model]
default_model = "claude-sonnet-4-20250514"

[context]
max_turns = 30
```

---

## Supported providers

| Provider | Env var | Example model |
|----------|---------|---------------|
| Anthropic | `ANTHROPIC_API_KEY` | `claude-sonnet-4-20250514` |
| OpenAI | `OPENAI_API_KEY` | `gpt-4o` |
| DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| Google | `GOOGLE_API_KEY` | `gemini-2.5-flash` |
| OpenRouter | `OPENROUTER_API_KEY` | `anthropic/claude-sonnet-4-20250514` |
| xAI | `XAI_API_KEY` | `grok-3` |
| Cerebras | `CEREBRAS_API_KEY` | `llama-4-scout-17b-16e-instruct` |
| Groq | `GROQ_API_KEY` | `llama-4-scout-17b-16e-instruct` |
| Mistral | `MISTRAL_API_KEY` | `mistral-large-latest` |
| Ollama | — | `qwen2.5-coder:14b` |
| OpenAI-compatible | `OPENAI_API_KEY` | Any (set `--base-url`) |

### Local models

Works with any OpenAI-compatible endpoint (vLLM, Ollama, LMStudio, etc.):

```sh
# vLLM
pipit --provider openai_compatible --base-url http://localhost:8000 --model Qwen/Qwen3.5-35B-A3B-FP8 --api-key dummy

# Ollama
pipit --provider ollama --model qwen2.5-coder:14b
```

---

## Authentication

```sh
# Store an API key
pipit auth login openai --api-key sk-...

# Use OAuth device flow (if supported)
pipit auth login google --device

# Check status
pipit auth status

# Remove credentials
pipit auth logout openai
```

Credentials are stored in `~/.pipit/credentials.json`.

---

## Single-shot mode

Pass a prompt directly for non-interactive use:

```sh
# Ask a question
pipit "what does the main function do?"

# Fix something with limited turns
pipit --max-turns 5 "fix the failing test in src/auth.rs"

# Use in CI/scripts
pipit --approval full_auto --max-turns 10 "run the tests and fix any failures"
```

---

## Key features

### RepoMap
Pipit automatically indexes your project to understand file structure, symbols, and dependencies. This gives the agent project-wide awareness without reading every file.

### Proof packets
Every run produces a proof packet — a structured record of what the agent planned, what it did, and what confidence it has. Use `/plans` to review them.

### Esc to cancel
Press **Esc** any time while the agent is running to stop it immediately. The timeline shows "⏹ Stopped" and you can continue with a new prompt.

### Context management
- `/add <file>` and `/drop <file>` to manage the working set
- `/compact` to compress context when tokens run low
- `/context` to see what's loaded
- Token usage shown in the status bar

### Skills, agents, and hooks
Place files in `.pipit/` for project-specific customization:
- `.pipit/skills/` — reusable instructions the agent can load
- `.pipit/agents/` — custom agent definitions
- `.pipit/hooks/` — lifecycle hooks (on session start, before edit, etc.)
- `.pipit/rules/` — project rules and constraints

---

## Advanced features

### Multi-agent mesh (`pipit-mesh`)

Distribute tasks across multiple pipit instances using a gossip-based mesh network. Nodes discover each other via the SWIM protocol, resolve conflicts with CRDTs, and delegate tasks based on capability scoring and affinity rules.

```sh
/mesh status          # Show discovered mesh nodes
/mesh delegate <task> # Delegate a task to the best-fit node
```

### MCP & A2A protocol (`pipit-mcp`)

Connect to external tool servers using the Model Context Protocol (MCP). Pipit also implements Google's Agent-to-Agent (A2A) protocol for inter-agent communication.

```sh
/mcp status           # Show connected MCP servers
/mcp tools            # List available MCP tools
```

Configure MCP servers in `.pipit/mcp.json`:
```json
{
  "servers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/path/to/dir"]
    }
  }
}
```

### Plugin marketplace (`pipit-mcp`)

Install and manage community extensions:

```sh
/plugins list         # List installed plugins
/plugins install <path>  # Install a plugin from a directory
/plugins uninstall <name>
```

### LSP integration (`pipit-lsp`)

IDE-grade code intelligence through Language Server Protocol. Pipit auto-detects and manages LSP servers for your project languages, providing goto-definition, find-references, and hover info to the agent.

### Benchmarking (`pipit-bench`)

Built-in benchmarking framework for evaluating agent performance on SWE-bench style tasks:

```sh
/bench run <suite>    # Run a benchmark suite
/bench results        # Show latest results
/bench compare        # Compare runs side-by-side
```

### Browser automation (`pipit-browser`)

Headless browser control for web testing, with visual regression via SSIM-based screenshot comparison:

```sh
/browse <url>         # Open a URL in headless browser
/browse screenshot    # Take a screenshot
/browse visual-diff   # Compare against baseline
```

### Dependency management (`pipit-deps`)

Cross-ecosystem dependency analysis and management:

```sh
/deps audit           # Security audit of dependencies
/deps outdated        # Show outdated packages
/deps tree            # Dependency tree visualization
```

### File watcher (`pipit-intelligence`)

Ambient file monitoring that detects changes and proactively suggests actions (e.g., run tests when test files change, update deps when manifests change):

```sh
/watch start          # Start watching for file changes
/watch stop           # Stop the watcher
/watch deps           # Watch dependency manifests
```

### Federated knowledge (`pipit-context`)

Cross-repository learning with privacy-preserving federation. Pipit can merge knowledge across repos with TF-IDF similarity matching and configurable federation policies.

### Self-healing CI (`pipit-daemon`)

Automatically classify and fix CI pipeline failures. Supports build errors, test failures, lint issues, deploy problems, and type-check errors with auto-generated fix branches.

### Cost oracle (`pipit-core`)

Predictive task cost estimation from historical data. Before running a task, get an estimate of token usage, cost, and time based on similar past tasks.

### Speculative execution (`pipit-core`)

Race multiple solution strategies in parallel using git worktrees — MinimalPatch, RootCause, and TestFirst — and pick the winner.

---

## CLI reference

```
pipit [OPTIONS] [PROMPT] [COMMAND]

Arguments:
  [PROMPT]    Initial prompt (runs non-interactively)

Commands:
  setup       Interactive setup wizard
  auth        Manage provider authentication
  update      Update pipit to the latest version

Options:
  -p, --provider <PROVIDER>    LLM provider
  -m, --model <MODEL>          Model name
      --api-key <KEY>          API key
  -a, --approval <MODE>        suggest, auto_edit, full_auto
      --root <PATH>            Project root
      --base-url <URL>         Custom LLM endpoint
      --mode <MODE>            fast, balanced, guarded, custom
      --max-turns <N>          Max agent turns per prompt
      --classic                Use classic REPL instead of TUI
      --thinking               Show model reasoning (default: true)
      --trace-ui               Show detailed tool traces
  -h, --help                   Print help
  -V, --version                Print version
```

---

## License

MIT