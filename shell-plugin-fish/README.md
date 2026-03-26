# Pipit Fish Shell Plugin 🐟

An AI-augmented shell integration for the pipit CLI, built for **Fish shell** —
leveraging Fish's native strengths that no other shell offers:

| Fish Native Feature | How the Plugin Uses It |
|---|---|
| **Universal variables** (`set -U`) | Session state persists across all terminals and restarts — zero file I/O |
| **Autoloaded functions** | Each command is one file in `functions/` — lazy-loaded, zero startup cost |
| **Native autosuggestions** | Fish's ghost-text suggestions work seamlessly alongside the plugin |
| **Declarative completions** | One `complete` statement per subcommand — with descriptions |
| **Abbreviations** | `fs` expands inline to `pipit suggest` — user sees the full command |
| **`commandline` builtin** | Direct buffer manipulation for key bindings — no ZLE widget dance |
| **Event system** | `--on-event fish_postexec` tracks last command for `:suggest` context |
| **`string` builtin** | `string match`, `string replace` — no sed/awk for parsing |
| **Built-in syntax highlighting** | No extra plugin required |

## Quick Install

```fish
# Automatic: creates symlinks in ~/.config/fish/
fish /path/to/pipit-cli/shell-plugin-fish/setup.fish

# Or manual: symlink the three directories
ln -s /path/to/shell-plugin-fish/conf.d/pipit.fish ~/.config/fish/conf.d/
ln -s /path/to/shell-plugin-fish/functions/*.fish ~/.config/fish/functions/
ln -s /path/to/shell-plugin-fish/completions/pipit.fish ~/.config/fish/completions/
exec fish
```

## Commands

```
pipit <prompt>              Send prompt to active agent
pipit new [agent]           Start new conversation
pipit info                  Show session info
pipit env                   Show environment & masked API keys
pipit suggest <desc>        Natural language → shell command (placed in buffer)
pipit commit                AI commit message + git commit
pipit commit-preview        Preview commit message only
pipit model <name|reset>    Set/clear model override
pipit provider <name>       Set/clear provider override
pipit agent <name>          Set/clear agent override
pipit conversation          Show current conversation
pipit conversation -        Switch to previous (like cd -)
pipit auth status           Show stored credentials
pipit auth login <p>        Login to provider
pipit auth logout <p>       Logout from provider
pipit copy                  Copy last response to clipboard
pipit doctor                Run diagnostics
pipit keyboard              Show key bindings
pipit help                  Show all commands
```

## Abbreviations (expand with Space)

| Type | Expands To |
|---|---|
| `fn` | `pipit new` |
| `fs` | `pipit suggest` |
| `fi` | `pipit info` |
| `fc` | `pipit commit` |
| `fe` | `pipit env` |
| `fh` | `pipit help` |
| `fco` | `pipit conversation` |
| `fm` | `pipit model` |
| `fp` | `pipit provider` |
| `fa` | `pipit auth` |

Unlike aliases, abbreviations expand **inline** — you see the full command before executing.

## Key Bindings

| Key | Action |
|---|---|
| **Ctrl+F** | AI suggest: takes current buffer text → generates shell command → replaces buffer |
| **Ctrl+X, Ctrl+F** | Fuzzy file picker (fzf) → inserts `@[path]` at cursor |
| **Alt+Enter** | Send current buffer as prompt to pipit agent |

## Tab Completion

Fish completions are declarative and show descriptions:

```
$ pipit ⇥
commit           AI commit message + commit
commit-preview   Preview commit message only
conversation     Switch conversations
doctor           Run diagnostics
...
```

```
$ pipit provider ⇥
anthropic   Anthropic (Claude)
deepseek    DeepSeek
google      Google (Gemini)
groq        Groq
...
```

## Universal Variables (Persistent State)

These survive terminal restarts — no config files needed:

| Variable | Purpose |
|---|---|
| `pipit_conversation_id` | Current conversation |
| `pipit_prev_conversation` | Previous conversation (for `pipit conversation -`) |
| `pipit_model` | Model override |
| `pipit_provider` | Provider override |
| `pipit_agent` | Agent override |

Inspect: `set -S pipit_model`
Clear: `set -eU pipit_model`

## File Structure

```
shell-plugin-fish/
├── conf.d/
│   └── pipit.fish              # Auto-loaded: env detection, abbreviations, bindings, hooks
├── functions/
│   ├── pipit.fish              # Main dispatcher (switch/case routing)
│   ├── pipit_new.fish          # Start new conversation
│   ├── pipit_info.fish         # Session info display
│   ├── pipit_env.fish          # Environment & API keys
│   ├── pipit_suggest.fish      # NL → shell command
│   ├── pipit_commit.fish       # AI commit message
│   ├── pipit_commit_preview.fish
│   ├── pipit_model.fish        # Model switching
│   ├── pipit_provider.fish     # Provider switching
│   ├── pipit_agent.fish        # Agent switching
│   ├── pipit_conversation.fish # Conversation management
│   ├── pipit_auth.fish         # Credential management
│   ├── pipit_copy.fish         # Clipboard copy
│   ├── pipit_help.fish         # Help text
│   ├── pipit_keyboard.fish     # Key binding help
│   ├── pipit_doctor.fish       # Diagnostics
│   ├── fish_right_prompt.fish  # RPROMPT with pipit status
│   ├── pipit_send.fish         # Send prompt to agent
│   ├── _pipit_exec.fish        # Binary wrapper (injects overrides)
│   ├── _pipit_log.fish         # Colored logging (✓✗⚠·)
│   ├── _pipit_fzf.fish         # Styled fzf wrapper
│   ├── _pipit_ensure_conversation.fish
│   ├── _pipit_keybind_suggest.fish     # Ctrl+F handler
│   ├── _pipit_keybind_file_picker.fish # Ctrl+X,F handler
│   └── _pipit_keybind_send_prompt.fish # Alt+Enter handler
├── completions/
│   └── pipit.fish              # Declarative completions with descriptions
├── setup.fish                  # One-command installer (symlinks)
└── README.md
```

Every function is **one file** — Fish autoloads on first call. Zero startup cost.

## Dependencies

**Required:**
- Fish shell (3.0+)
- `pipit` binary (in PATH or set `PIPIT_BIN`)

**Optional:**
- `fzf` — fuzzy finder for Ctrl+X,F file picker
- `fd` / `fdfind` — fast file listing (falls back to `find`)
- `bat` / `batcat` — syntax-highlighted preview (falls back to `cat`)
