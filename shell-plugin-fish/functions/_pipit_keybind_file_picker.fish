# ──────────────────────────────────────────────────────────────────────
#  _pipit_keybind_file_picker — Ctrl+X,Ctrl+F: fzf file picker → @[path]
# ──────────────────────────────────────────────────────────────────────
#
#  Opens fzf to pick a file, then inserts @[path] at the cursor position.
#  Uses fd for fast listing, bat for preview.
#
# ──────────────────────────────────────────────────────────────────────
function _pipit_keybind_file_picker
    if test -z "$_pipit_has_fzf"
        commandline -f repaint
        return
    end

    # Build file listing command
    set -l finder "find . -type f -not -path '*/.*' -not -path '*/target/*' -not -path '*/node_modules/*'"
    if test -n "$_pipit_has_fd"
        set finder "fd --type f --hidden --exclude .git --exclude target --exclude node_modules"
    end

    # Build preview command
    set -l previewer "cat {}"
    if test -n "$_pipit_has_bat"
        if command -sq bat
            set previewer "bat --style=numbers --color=always --line-range=:80 {}"
        else
            set previewer "batcat --style=numbers --color=always --line-range=:80 {}"
        end
    end

    set -l selected (eval $finder 2>/dev/null | _pipit_fzf --preview "$previewer" --prompt "File> ")

    if test -n "$selected"
        # Insert @[path] at cursor
        commandline -i "@[$selected]"
    end

    commandline -f repaint
end
