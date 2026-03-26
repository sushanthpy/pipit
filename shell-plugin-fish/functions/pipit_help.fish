# ──────────────────────────────────────────────────────────────────────
#  pipit_help — Show all pipit commands
# ──────────────────────────────────────────────────────────────────────
function pipit_help -d "Show pipit shell plugin commands"
    echo
    set_color --bold cyan
    echo "  Pipit Shell Plugin — Commands"
    set_color normal
    echo

    set_color --bold
    echo "  Core"
    set_color normal
    printf "    %-28s %s\n" "pipit <prompt>"           "Send prompt to active agent"
    printf "    %-28s %s\n" "pipit new [agent]"        "Start new conversation"
    printf "    %-28s %s\n" "pipit info"               "Show session info"
    printf "    %-28s %s\n" "pipit env"                "Show environment & API keys"
    printf "    %-28s %s\n" "pipit copy"               "Copy last response to clipboard"
    echo

    set_color --bold
    echo "  AI Actions"
    set_color normal
    printf "    %-28s %s\n" "pipit suggest <desc>"        "Natural language → shell command"
    printf "    %-28s %s\n" "pipit suggest --explain ..." "Include explanation with command"
    printf "    %-28s %s\n" "pipit suggest --multi ..."   "Multiple alternatives (fzf pick)"
    printf "    %-28s %s\n" "pipit commit"                "AI commit message + commit"
    printf "    %-28s %s\n" "pipit commit-preview"        "Preview commit message only"
    printf "    %-28s %s\n" "pipit data -i f.jsonl -t .." "JSONL batch processing"
    echo

    set_color --bold
    echo "  Configuration"
    set_color normal
    printf "    %-28s %s\n" "pipit model <name|reset>" "Set/clear model override"
    printf "    %-28s %s\n" "pipit provider <name>"    "Set/clear provider override"
    printf "    %-28s %s\n" "pipit agent <name>"       "Set/clear agent override"
    echo

    set_color --bold
    echo "  SuggestConfig (optional — use cheaper model for suggestions)"
    set_color normal
    printf "    %-28s %s\n" "set -U pipit_suggest_model"    "e.g. gpt-4o-mini"
    printf "    %-28s %s\n" "set -U pipit_suggest_provider" "e.g. openai"
    echo

    set_color --bold
    echo "  Conversations"
    set_color normal
    printf "    %-28s %s\n" "pipit conversation"       "Show current conversation"
    printf "    %-28s %s\n" "pipit conversation -"     "Switch to previous (like cd -)"
    printf "    %-28s %s\n" "pipit conversation <id>"  "Switch to specific conversation"
    echo

    set_color --bold
    echo "  Sandbox (safe experimentation)"
    set_color normal
    printf "    %-28s %s\n" "pipit sandbox"            "Create git worktree sandbox"
    printf "    %-28s %s\n" "pipit sandbox exit"       "Return + cleanup worktree"
    printf "    %-28s %s\n" "pipit sandbox status"     "Show sandbox info"
    printf "    %-28s %s\n" "pipit sandbox keep"       "Exit but keep worktree"
    echo

    set_color --bold
    echo "  Undo (file change tracking)"
    set_color normal
    printf "    %-28s %s\n" "pipit undo track <file>"  "Snapshot before editing"
    printf "    %-28s %s\n" "pipit undo"               "Restore last snapshot (fzf)"
    printf "    %-28s %s\n" "pipit undo list"          "Show undo stack"
    printf "    %-28s %s\n" "pipit undo clear"         "Clear undo history"
    echo

    set_color --bold
    echo "  Todo (task tracking — syncs across terminals)"
    set_color normal
    printf "    %-28s %s\n" "pipit todo"               "List all todos"
    printf "    %-28s %s\n" "pipit todo add <text>"    "Add a task"
    printf "    %-28s %s\n" "pipit todo doing <n>"     "Mark as in-progress"
    printf "    %-28s %s\n" "pipit todo done <n>"      "Mark as complete"
    printf "    %-28s %s\n" "pipit todo rm <n>"        "Remove a task"
    echo

    set_color --bold
    echo "  Diff"
    set_color normal
    printf "    %-28s %s\n" "pipit diff"               "Colored git diff"
    printf "    %-28s %s\n" "pipit diff --staged"      "Staged changes only"
    printf "    %-28s %s\n" "pipit diff file1 file2"   "Compare two files"
    echo

    set_color --bold
    echo "  Auth"
    set_color normal
    printf "    %-28s %s\n" "pipit auth status"        "Show stored credentials"
    printf "    %-28s %s\n" "pipit auth login <p>"     "Login to provider"
    printf "    %-28s %s\n" "pipit auth logout <p>"    "Logout from provider"
    echo

    set_color --bold
    echo "  Diagnostics"
    set_color normal
    printf "    %-28s %s\n" "pipit doctor"             "Run diagnostics"
    printf "    %-28s %s\n" "pipit keyboard"           "Show key bindings"
    printf "    %-28s %s\n" "pipit help"               "This help"
    echo

    set_color --bold
    echo "  Abbreviations (expand with Space)"
    set_color normal
    printf "    %-8s → %s\n" "fn"  "pipit new"
    printf "    %-8s → %s\n" "fs"  "pipit suggest"
    printf "    %-8s → %s\n" "fi"  "pipit info"
    printf "    %-8s → %s\n" "fc"  "pipit commit"
    printf "    %-8s → %s\n" "fe"  "pipit env"
    printf "    %-8s → %s\n" "fh"  "pipit help"
    printf "    %-8s → %s\n" "fco" "pipit conversation"
    printf "    %-8s → %s\n" "fm"  "pipit model"
    printf "    %-8s → %s\n" "fp"  "pipit provider"
    printf "    %-8s → %s\n" "fa"  "pipit auth"
    printf "    %-8s → %s\n" "fsd" "pipit sandbox"
    printf "    %-8s → %s\n" "fu"  "pipit undo"
    printf "    %-8s → %s\n" "ft"  "pipit todo"
    printf "    %-8s → %s\n" "fdf" "pipit diff"
    echo
end
