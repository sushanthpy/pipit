# ──────────────────────────────────────────────────────────────────────
#  pipit.fish — Auto-loaded configuration for the Pipit Fish shell plugin
# ──────────────────────────────────────────────────────────────────────
#
#  Fish auto-sources everything in conf.d/ on startup.
#  Functions in functions/ are lazy-loaded on first call (zero startup cost).
#
#  Usage:
#    pipit new             → Start new conversation
#    pipit suggest ...     → Natural language → shell command
#    pipit commit          → AI commit message
#    @file                 → Tag files for context (via key binding)
#    Ctrl+F                → AI suggest from current line
#
#  Install:
#    See shell-plugin-fish/setup.fish
#
# ──────────────────────────────────────────────────────────────────────

# Guard: only load once per shell session
set -q _pipit_plugin_loaded; and return
set -g _pipit_plugin_loaded 1

# ── Resolve plugin root ──
set -g _pipit_plugin_dir (status dirname)/..

# ── Locate pipit binary ──
if set -q PIPIT_BIN
    set -g _pipit_bin $PIPIT_BIN
else if command -sq pipit
    set -g _pipit_bin pipit
else
    set -g _pipit_bin ""
end

# ── Detect optional tools (lazy — checked once) ──
set -g _pipit_has_fzf  (command -sq fzf;  and echo 1; or echo "")
set -g _pipit_has_fd   (command -sq fd;   and echo 1; or begin; command -sq fdfind; and echo 1; or echo ""; end)
set -g _pipit_has_bat  (command -sq bat;  and echo 1; or begin; command -sq batcat; and echo 1; or echo ""; end)

# ── Universal variables for persistent state (survive restarts) ──
# Only initialize if not already set — universal vars persist automatically.
set -q -U pipit_conversation_id;   or set -U pipit_conversation_id ""
set -q -U pipit_prev_conversation; or set -U pipit_prev_conversation ""
set -q -U pipit_model;             or set -U pipit_model ""
set -q -U pipit_provider;          or set -U pipit_provider ""
set -q -U pipit_agent;             or set -U pipit_agent ""

# SuggestConfig — optional dedicated cheaper/faster model for :suggest
# If set, suggest uses this model+provider instead of the main session ones.
set -q -U pipit_suggest_model;    or set -U pipit_suggest_model ""
set -q -U pipit_suggest_provider; or set -U pipit_suggest_provider ""

# Sandbox state — tracks if we're inside a git worktree sandbox
set -q -U pipit_sandbox_origin;   or set -U pipit_sandbox_origin ""
set -q -U pipit_sandbox_worktree; or set -U pipit_sandbox_worktree ""

# Undo stack — fish universal list variable, each entry is "timestamp:path:backup_path"
set -q -U pipit_undo_stack;       or set -U pipit_undo_stack

# Todo list — universal list, persists across terminals
set -q -U pipit_todos;            or set -U pipit_todos

# Spinner state (global, not universal — per-session)
set -g _pipit_spinner_pid ""
set -g _pipit_spinner_active 0

# ── Abbreviations (expand inline — user sees the full command) ──
abbr -a -- fn   'pipit new'
abbr -a -- fs   'pipit suggest'
abbr -a -- fi   'pipit info'
abbr -a -- fc   'pipit commit'
abbr -a -- fcp  'pipit commit-preview'
abbr -a -- fe   'pipit env'
abbr -a -- fh   'pipit help'
abbr -a -- fdr  'pipit doctor'
abbr -a -- fco  'pipit conversation'
abbr -a -- fm   'pipit model'
abbr -a -- fp   'pipit provider'
abbr -a -- fa   'pipit auth'
abbr -a -- fsd  'pipit sandbox'
abbr -a -- fsx  'pipit sandbox exit'
abbr -a -- fu   'pipit undo'
abbr -a -- ft   'pipit todo'
abbr -a -- fdg  'pipit data'
abbr -a -- fdf  'pipit diff'

# ── Key bindings ──
# Ctrl+F: Take current command line text → AI suggest → place result in buffer
bind \cf _pipit_keybind_suggest
# Ctrl+X Ctrl+F: Fuzzy file picker → insert @[path] at cursor
bind \cx\cf _pipit_keybind_file_picker
# Alt+Enter: Send current line as prompt to pipit agent
bind \e\r _pipit_keybind_send_prompt
# Ctrl+X Ctrl+M: Model selector via fzf
bind \cx\cm _pipit_keybind_model_picker
# Ctrl+X Ctrl+U: Undo last file change
bind \cx\cu _pipit_keybind_undo

# ── Fish event hooks ──
function _pipit_on_postexec --on-event fish_postexec
    # Track last command for context (useful for :suggest after errors)
    set -g _pipit_last_command $argv[1]
    set -g _pipit_last_status $status
end

# React to sandbox variable changes — update prompt automatically
function _pipit_on_sandbox_change --on-variable pipit_sandbox_worktree
    commandline -f repaint
end
