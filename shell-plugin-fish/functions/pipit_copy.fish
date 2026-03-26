# ──────────────────────────────────────────────────────────────────────
#  pipit_copy — Copy last AI response to clipboard
# ──────────────────────────────────────────────────────────────────────
function pipit_copy -d "Copy last pipit response to clipboard"
    if not set -q _pipit_last_response
        _pipit_log error "No response to copy."
        return 1
    end

    # Detect clipboard command
    if command -sq pbcopy
        echo $_pipit_last_response | pbcopy
    else if command -sq xclip
        echo $_pipit_last_response | xclip -selection clipboard
    else if command -sq xsel
        echo $_pipit_last_response | xsel --clipboard
    else if command -sq wl-copy
        echo $_pipit_last_response | wl-copy
    else
        _pipit_log error "No clipboard utility found (pbcopy, xclip, xsel, wl-copy)."
        return 1
    end

    _pipit_log ok "Copied to clipboard."
end
