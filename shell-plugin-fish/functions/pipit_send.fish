# ──────────────────────────────────────────────────────────────────────
#  pipit_send — Send a prompt to the active pipit agent
# ──────────────────────────────────────────────────────────────────────
function pipit_send -d "Send prompt to pipit agent"
    if test (count $argv) -eq 0
        _pipit_log error "No prompt provided."
        return 1
    end

    _pipit_ensure_conversation
    set -l prompt (string join ' ' $argv)

    # If the prompt contains @[file] references, expand them
    # Fish's string match extracts the file paths
    set -l files
    for token in $argv
        if string match -qr '^@\[(.+)\]$' -- $token
            set -l path (string match -r '^@\[(.+)\]$' -- $token)[2]
            if test -f "$path"
                set -a files --file $path
            else
                _pipit_log warn "File not found: $path"
            end
        end
    end

    _pipit_exec --tui $files -- $prompt
end
