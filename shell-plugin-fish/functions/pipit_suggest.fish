# ──────────────────────────────────────────────────────────────────────
#  pipit_suggest — Natural language → shell command
# ──────────────────────────────────────────────────────────────────────
#
#  SuggestConfig: Uses dedicated model/provider if set, otherwise falls
#  back to the main session model. This lets you use a cheaper/faster
#  model for suggestions while keeping an expensive model for the agent.
#
#    set -U pipit_suggest_model  gpt-4o-mini       # fast + cheap
#    set -U pipit_suggest_provider openai
#
#  The AI returns a shell command. It is placed in the command line
#  buffer for review — NOT auto-executed (safe by design).
#
#  Usage:
#    pipit suggest list all rust files larger than 1MB
#    fs find docker containers using port 8080       (via abbreviation)
#    Ctrl+F                                          (from typed text)
#
#  Flags:
#    --explain     Also print a brief explanation above the command
#    --multi       Return multiple alternatives (pick via fzf)
#
# ──────────────────────────────────────────────────────────────────────
function pipit_suggest -d "AI: natural language → shell command"
    # Parse flags
    set -l explain 0
    set -l multi 0
    set -l words

    for arg in $argv
        switch $arg
            case --explain
                set explain 1
            case --multi
                set multi 1
            case '--*'
                _pipit_log error "Unknown flag: $arg"
                return 1
            case '*'
                set -a words $arg
        end
    end

    if test (count $words) -eq 0
        _pipit_log error "Describe what you want in natural language."
        echo "  Usage: pipit suggest [--explain] [--multi] <description>"
        echo
        echo "  Examples:"
        echo "    pipit suggest list all rust files larger than 1MB"
        echo "    pipit suggest --explain find processes on port 8080"
        echo "    pipit suggest --multi ways to compress a directory"
        echo
        echo "  SuggestConfig (optional — set once, persists forever):"
        echo "    set -U pipit_suggest_model gpt-4o-mini"
        echo "    set -U pipit_suggest_provider openai"
        return 1
    end

    set -l description (string join ' ' $words)

    # ── Build rich context ──
    set -l ctx_parts \
        "Shell: fish" \
        "OS: "(uname -s) \
        "Arch: "(uname -m) \
        "CWD: "(pwd)

    # Git context if in a repo
    if git rev-parse --is-inside-work-tree >/dev/null 2>&1
        set -a ctx_parts "Git branch: "(git branch --show-current 2>/dev/null)
        set -l dirty (git status --porcelain 2>/dev/null | wc -l | string trim)
        if test "$dirty" != "0"
            set -a ctx_parts "Git dirty: $dirty files"
        end
    end

    # Include last failed command for "fix this" patterns
    if test -n "$_pipit_last_command" -a "$_pipit_last_status" != "0"
        set -a ctx_parts "Last failed: '$_pipit_last_command' (exit $_pipit_last_status)"
    end

    # Detect common tools available
    set -l tools_available
    for tool in docker kubectl npm cargo python3 go java brew apt
        if command -sq $tool
            set -a tools_available $tool
        end
    end
    if test (count $tools_available) -gt 0
        set -a ctx_parts "Available tools: "(string join ', ' $tools_available)
    end

    set -l context (string join ' | ' $ctx_parts)

    # ── Build system prompt based on flags ──
    set -l system_prompt
    if test $multi -eq 1
        set system_prompt "You are a shell command generator for fish shell. The user describes what they want. Return EXACTLY 5 alternative commands, one per line, numbered 1-5. Each line: NUMBER. COMMAND — BRIEF_DESCRIPTION. No markdown fencing. Context: $context"
    else if test $explain -eq 1
        set system_prompt "You are a shell command generator for fish shell. Return the command on the FIRST line, then a blank line, then a 1-2 sentence explanation. No markdown fencing. Context: $context"
    else
        set system_prompt "You are a shell command generator for fish shell. Return ONLY the shell command — no explanation, no markdown, no backticks, no line numbers. Context: $context"
    end

    # ── Resolve model: SuggestConfig overrides session overrides ──
    set -l suggest_args
    if test -n "$pipit_suggest_model"
        set -a suggest_args --model $pipit_suggest_model
    end
    if test -n "$pipit_suggest_provider"
        set -a suggest_args --provider $pipit_suggest_provider
    end

    # Show spinner
    _pipit_spinner_start "Suggesting"

    set -l result (_pipit_exec $suggest_args prompt --system "$system_prompt" "$description" 2>/dev/null)
    set -l exit_code $status

    _pipit_spinner_stop

    if test $exit_code -ne 0 -o -z "$result"
        _pipit_log error "No suggestion received."
        return 1
    end

    # Strip any accidental markdown fencing
    set result (string replace -r '^```[a-z]*\n?' '' -- $result)
    set result (string replace -r '\n?```$' '' -- $result)

    # ── Handle --multi: pipe alternatives through fzf ──
    if test $multi -eq 1
        if test -z "$_pipit_has_fzf"
            # No fzf — just print all alternatives
            echo
            set_color --bold yellow
            echo "  Alternatives:"
            set_color normal
            echo $result | sed 's/^/    /'
            echo
            _pipit_log info "Install fzf to pick interactively."
            return 0
        end

        # Parse: pick via fzf, extract the command part
        set -l picked (echo $result | _pipit_fzf --prompt "Pick> " --header "Select a command")
        if test -z "$picked"
            _pipit_log info "Cancelled."
            return 0
        end

        # Strip "N. " prefix and " — description" suffix
        set result (string replace -r '^\d+\.\s*' '' -- $picked)
        set result (string replace -r '\s*—\s*.*$' '' -- $result)
    end

    # ── Handle --explain: split command from explanation ──
    if test $explain -eq 1 -a $multi -eq 0
        set -l lines (string split \n -- $result)
        set -l cmd (string trim -- $lines[1])

        echo
        set_color --bold yellow
        echo "  ⚡ $cmd"
        set_color normal

        # Print explanation lines (skip blank line after command)
        if test (count $lines) -gt 1
            set_color brblack
            for line in $lines[2..-1]
                if test -n (string trim -- "$line")
                    echo "    $line"
                end
            end
            set_color normal
        end
        echo

        commandline -r $cmd
        commandline -f repaint
        return
    end

    # ── Default: single command → buffer ──
    set result (string trim -- $result)

    echo
    set_color --bold yellow
    echo "  ⚡ $result"
    set_color normal
    echo

    commandline -r $result
    commandline -f repaint
end
