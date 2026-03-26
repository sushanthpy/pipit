# ──────────────────────────────────────────────────────────────────────
#  pipit_undo — File change undo via tracked backup stack
# ──────────────────────────────────────────────────────────────────────
#
#  Fish-native approach:
#    - Universal list variable `pipit_undo_stack` persists across sessions
#    - Each entry: "timestamp|original_path|backup_path"
#    - `pipit track <file>` snapshots before editing
#    - `pipit undo` restores last snapshot (or pick via fzf)
#
#  The undo stack auto-prunes entries older than 24 hours.
#
# ──────────────────────────────────────────────────────────────────────
function pipit_undo -d "Undo file changes from tracked snapshots"
    set -l subcmd $argv[1]

    switch "$subcmd"
        case track
            # Track a file: snapshot its current content
            if test (count $argv) -lt 2
                _pipit_log error "Usage: pipit undo track <file>"
                return 1
            end
            _pipit_undo_track $argv[2..-1]

        case list ls
            _pipit_undo_list

        case clear
            _pipit_undo_clear

        case '' pop
            # Default: undo the last tracked change
            _pipit_undo_pop

        case '*'
            # If arg looks like a file path, treat as track
            if test -f "$subcmd"
                _pipit_undo_track $argv
            else
                _pipit_log error "Unknown undo command: $subcmd"
                echo "  Usage: pipit undo [track <file> | list | clear]"
            end
    end
end

function _pipit_undo_track -d "Snapshot a file before changes"
    for filepath in $argv
        if not test -f "$filepath"
            _pipit_log warn "File not found: $filepath"
            continue
        end

        set -l abs_path (realpath $filepath)
        set -l timestamp (date +%s)
        set -l backup_dir "$HOME/.pipit/undo"
        mkdir -p $backup_dir

        # Backup filename: hash of path + timestamp
        set -l hash (echo -n "$abs_path-$timestamp" | shasum -a 256 | string sub -l 12)
        set -l backup_path "$backup_dir/$hash"

        command cp -p $abs_path $backup_path

        # Push to universal undo stack
        set -U -a pipit_undo_stack "$timestamp|$abs_path|$backup_path"

        _pipit_log ok "Tracked: "(string replace $HOME '~' -- $abs_path)
    end

    # Auto-prune entries older than 24h
    _pipit_undo_prune
end

function _pipit_undo_pop -d "Restore the last tracked file"
    if test (count $pipit_undo_stack) -eq 0
        _pipit_log info "Undo stack is empty."
        return 1
    end

    # If fzf available and stack has multiple entries, offer selection
    if test -n "$_pipit_has_fzf" -a (count $pipit_undo_stack) -gt 1
        set -l display_lines
        set -l i 0
        for entry in $pipit_undo_stack
            set i (math $i + 1)
            set -l parts (string split '|' -- $entry)
            set -l ts $parts[1]
            set -l path $parts[2]
            set -l age (math (date +%s) - $ts)
            set -l age_str (_pipit_format_duration $age)
            set -a display_lines (printf "%d  %-50s  %s ago" $i (string replace $HOME '~' -- $path) $age_str)
        end

        set -l picked (printf '%s\n' $display_lines | \
            _pipit_fzf --prompt "Undo> " --header "# FILE  AGE" --tac)

        if test -z "$picked"
            _pipit_log info "Cancelled."
            return
        end

        set -l idx (string split -m1 ' ' -- (string trim -- $picked))[1]
        _pipit_undo_restore $idx
    else
        # Pop the last entry
        _pipit_undo_restore (count $pipit_undo_stack)
    end
end

function _pipit_undo_restore -d "Restore entry at index"
    set -l idx $argv[1]

    if test $idx -lt 1 -o $idx -gt (count $pipit_undo_stack)
        _pipit_log error "Invalid undo index: $idx"
        return 1
    end

    set -l entry $pipit_undo_stack[$idx]
    set -l parts (string split '|' -- $entry)
    set -l original_path $parts[2]
    set -l backup_path $parts[3]

    if not test -f "$backup_path"
        _pipit_log error "Backup file missing: $backup_path"
        # Remove stale entry
        set -U -e pipit_undo_stack[$idx]
        return 1
    end

    # Show diff before restoring
    if command -sq diff
        echo
        set_color brblack
        echo "  Changes to undo:"
        set_color normal
        diff --color=always -u $backup_path $original_path 2>/dev/null | head -30 | sed 's/^/    /'
        echo
    end

    # Confirm
    read -P (set_color yellow)"  Restore "(string replace $HOME '~' -- $original_path)"? [Y/n] "(set_color normal) -l choice
    if string match -qi 'n' -- $choice
        _pipit_log info "Cancelled."
        return
    end

    command cp -p $backup_path $original_path
    _pipit_log ok "Restored: "(string replace $HOME '~' -- $original_path)

    # Remove entry and backup
    set -U -e pipit_undo_stack[$idx]
    command rm -f $backup_path
end

function _pipit_undo_list -d "Show undo stack"
    if test (count $pipit_undo_stack) -eq 0
        _pipit_log info "Undo stack is empty."
        return
    end

    echo
    set_color --bold cyan
    echo "  Undo Stack ("(count $pipit_undo_stack)" entries)"
    set_color normal
    echo

    set -l i 0
    for entry in $pipit_undo_stack
        set i (math $i + 1)
        set -l parts (string split '|' -- $entry)
        set -l ts $parts[1]
        set -l path $parts[2]
        set -l age (math (date +%s) - $ts)
        set -l age_str (_pipit_format_duration $age)
        printf "  %2d. %-50s  %s ago\n" $i (string replace $HOME '~' -- $path) $age_str
    end
    echo
end

function _pipit_undo_clear -d "Clear the undo stack"
    set -l count (count $pipit_undo_stack)
    if test $count -eq 0
        _pipit_log info "Stack already empty."
        return
    end

    read -P (set_color yellow)"  Clear $count undo entries? [y/N] "(set_color normal) -l choice
    if not string match -qi 'y' -- $choice
        return
    end

    # Remove backup files
    for entry in $pipit_undo_stack
        set -l parts (string split '|' -- $entry)
        command rm -f $parts[3]
    end

    set -U pipit_undo_stack
    _pipit_log ok "Cleared $count entries."
end

function _pipit_undo_prune -d "Remove entries older than 24h"
    set -l cutoff (math (date +%s) - 86400)
    set -l pruned 0
    set -l new_stack

    for entry in $pipit_undo_stack
        set -l parts (string split '|' -- $entry)
        set -l ts $parts[1]
        if test $ts -ge $cutoff
            set -a new_stack $entry
        else
            command rm -f $parts[3]
            set pruned (math $pruned + 1)
        end
    end

    if test $pruned -gt 0
        set -U pipit_undo_stack $new_stack
    end
end

function _pipit_format_duration -d "Format seconds as human-readable duration"
    set -l secs $argv[1]
    if test $secs -lt 60
        echo "{$secs}s"
    else if test $secs -lt 3600
        echo (math "floor($secs / 60)")"m"
    else
        echo (math "floor($secs / 3600)")"h"(math "floor($secs % 3600 / 60)")"m"
    end
end
