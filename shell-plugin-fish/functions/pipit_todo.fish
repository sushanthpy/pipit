# ──────────────────────────────────────────────────────────────────────
#  pipit_todo — Task tracking via universal list variables
# ──────────────────────────────────────────────────────────────────────
#
#  Fish-native approach:
#    - Universal list `pipit_todos` persists across all terminals
#    - Each entry: "status|text" where status is TODO/DOING/DONE
#    - Changes in one terminal are instantly visible in all others
#    - Right prompt can show active task count
#
#  Usage:
#    pipit todo                    → List all todos
#    pipit todo add <text>         → Add a new todo
#    pipit todo done <n>           → Mark #n as done
#    pipit todo doing <n>          → Mark #n as in-progress
#    pipit todo rm <n>             → Remove #n
#    pipit todo clear              → Remove all done items
#    pipit todo reset              → Remove everything
#
# ──────────────────────────────────────────────────────────────────────
function pipit_todo -d "Task tracking for pipit sessions"
    if test (count $argv) -eq 0
        _pipit_todo_list
        return
    end

    switch $argv[1]
        case add a new
            if test (count $argv) -lt 2
                _pipit_log error "Usage: pipit todo add <description>"
                return 1
            end
            set -l text (string join ' ' $argv[2..-1])
            set -U -a pipit_todos "TODO|$text"
            _pipit_log ok "Added: $text"

        case done d
            if test (count $argv) -lt 2
                _pipit_log error "Usage: pipit todo done <number>"
                return 1
            end
            _pipit_todo_set_status $argv[2] "DONE"

        case doing working w
            if test (count $argv) -lt 2
                _pipit_log error "Usage: pipit todo doing <number>"
                return 1
            end
            _pipit_todo_set_status $argv[2] "DOING"

        case rm remove del
            if test (count $argv) -lt 2
                _pipit_log error "Usage: pipit todo rm <number>"
                return 1
            end
            set -l idx $argv[2]
            if test $idx -lt 1 -o $idx -gt (count $pipit_todos)
                _pipit_log error "Invalid index: $idx (have "(count $pipit_todos)" items)"
                return 1
            end
            set -l entry $pipit_todos[$idx]
            set -l text (string split -m1 '|' -- $entry)[2]
            set -U -e pipit_todos[$idx]
            _pipit_log ok "Removed: $text"

        case clear
            # Remove all DONE items
            set -l new_todos
            set -l cleared 0
            for entry in $pipit_todos
                set -l status (string split -m1 '|' -- $entry)[1]
                if test "$status" = "DONE"
                    set cleared (math $cleared + 1)
                else
                    set -a new_todos $entry
                end
            end
            set -U pipit_todos $new_todos
            _pipit_log ok "Cleared $cleared completed items."

        case reset
            read -P (set_color yellow)"  Reset all "(count $pipit_todos)" todos? [y/N] "(set_color normal) -l choice
            if string match -qi 'y' -- $choice
                set -U pipit_todos
                _pipit_log ok "All todos cleared."
            end

        case '*'
            # Treat as shorthand for "add"
            set -l text (string join ' ' $argv)
            set -U -a pipit_todos "TODO|$text"
            _pipit_log ok "Added: $text"
    end
end

function _pipit_todo_list -d "Display todo list"
    if test (count $pipit_todos) -eq 0
        _pipit_log info "No todos. Add one: pipit todo add <description>"
        return
    end

    echo
    set_color --bold cyan
    echo "  Todo List"
    set_color normal
    echo

    set -l i 0
    for entry in $pipit_todos
        set i (math $i + 1)
        set -l parts (string split -m1 '|' -- $entry)
        set -l status $parts[1]
        set -l text $parts[2]

        switch $status
            case TODO
                set_color white
                printf "  %2d. ○ %s\n" $i $text
            case DOING
                set_color yellow
                printf "  %2d. ◉ %s\n" $i $text
            case DONE
                set_color brblack
                printf "  %2d. ✓ %s\n" $i $text
        end
        set_color normal
    end

    # Summary line
    set -l total (count $pipit_todos)
    set -l done 0
    set -l doing 0
    for entry in $pipit_todos
        set -l status (string split -m1 '|' -- $entry)[1]
        if test "$status" = "DONE"
            set done (math $done + 1)
        else if test "$status" = "DOING"
            set doing (math $doing + 1)
        end
    end

    echo
    set_color brblack
    printf "  %d/%d done" $done $total
    if test $doing -gt 0
        printf " · %d in progress" $doing
    end
    echo
    set_color normal
    echo
end

function _pipit_todo_set_status -d "Change todo status"
    set -l idx $argv[1]
    set -l new_status $argv[2]

    if test $idx -lt 1 -o $idx -gt (count $pipit_todos)
        _pipit_log error "Invalid index: $idx (have "(count $pipit_todos)" items)"
        return 1
    end

    set -l entry $pipit_todos[$idx]
    set -l text (string split -m1 '|' -- $entry)[2]
    set -U pipit_todos[$idx] "$new_status|$text"

    switch $new_status
        case DONE
            _pipit_log ok "Done: $text"
        case DOING
            _pipit_log info "Working: $text"
        case TODO
            _pipit_log info "Reset: $text"
    end
end
