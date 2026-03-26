# ──────────────────────────────────────────────────────────────────────
#  pipit_right_prompt — RPROMPT showing pipit status
# ──────────────────────────────────────────────────────────────────────
#
#  Fish uses `fish_right_prompt` function for the right side of the prompt.
#  Source this file to get a right-prompt showing active model/provider.
#
#  To use: add to ~/.config/fish/functions/fish_right_prompt.fish
#  Or: source this file from your config.fish
#
# ──────────────────────────────────────────────────────────────────────
function fish_right_prompt
    set -l parts

    # Show sandbox indicator if active
    if test -n "$pipit_sandbox_worktree"
        set -a parts (set_color --bold brmagenta)"🧪sandbox"(set_color normal)
    end

    # Show active todo count
    set -l doing_count 0
    set -l todo_count 0
    for entry in $pipit_todos
        set -l status (string split -m1 '|' -- $entry)[1]
        if test "$status" = "DOING"
            set doing_count (math $doing_count + 1)
        else if test "$status" = "TODO"
            set todo_count (math $todo_count + 1)
        end
    end
    if test $doing_count -gt 0
        set -a parts (set_color bryellow)"◉$doing_count"(set_color normal)
    else if test $todo_count -gt 0
        set -a parts (set_color brblack)"○$todo_count"(set_color normal)
    end

    # Show agent if set
    if test -n "$pipit_agent"
        set -a parts (set_color brmagenta)"⚡$pipit_agent"(set_color normal)
    end

    # Show model if overridden
    if test -n "$pipit_model"
        set -a parts (set_color brblue)"◈$pipit_model"(set_color normal)
    end

    # Show provider if overridden
    if test -n "$pipit_provider"
        set -a parts (set_color brcyan)"▸$pipit_provider"(set_color normal)
    end

    if test (count $parts) -gt 0
        set_color brblack
        echo -n "["
        set_color normal
        echo -n (string join " " $parts)
        set_color brblack
        echo -n "]"
        set_color normal
    end
end
