# ──────────────────────────────────────────────────────────────────────
#  _pipit_keybind_suggest — Ctrl+F: AI suggest from current buffer
# ──────────────────────────────────────────────────────────────────────
#
#  Takes whatever is currently typed in the command line and treats it
#  as a natural language description → generates a shell command.
#  Fish's `commandline` builtin gives us direct buffer access.
#
#  Uses SuggestConfig (pipit_suggest_model/pipit_suggest_provider) if set,
#  so you can use a cheaper/faster model for quick inline suggestions.
#
# ──────────────────────────────────────────────────────────────────────
function _pipit_keybind_suggest
    set -l buf (commandline -b)

    if test -z "$buf"
        commandline -f repaint
        return
    end

    # Show a spinner while thinking
    commandline -r "# ⠋ thinking..."
    commandline -f repaint

    # Use the buffer text as the description
    set -l context "Shell: fish | OS: "(uname -s)" | CWD: "(pwd)

    # Add last failed command context for "fix this" patterns
    if test -n "$_pipit_last_command" -a "$_pipit_last_status" != "0"
        set context "$context | Last failed: '$_pipit_last_command' (exit $_pipit_last_status)"
    end

    set -l system_prompt "You are a shell command generator for fish shell. Return ONLY the fish shell command — no explanation, no markdown, no backticks. Context: $context"

    # Use SuggestConfig model if set (cheaper/faster for keybind suggestions)
    set -l suggest_args
    if test -n "$pipit_suggest_model"
        set -a suggest_args --model $pipit_suggest_model
    end
    if test -n "$pipit_suggest_provider"
        set -a suggest_args --provider $pipit_suggest_provider
    end

    set -l result (_pipit_exec $suggest_args prompt --system "$system_prompt" "$buf" 2>/dev/null)

    if test -n "$result"
        # Clean up markdown fencing
        set result (string replace -r '^```[a-z]*\n?' '' -- $result)
        set result (string replace -r '\n?```$' '' -- $result)
        set result (string trim -- $result)
        commandline -r $result
    else
        # Restore original buffer on failure
        commandline -r $buf
    end

    commandline -f repaint
end
