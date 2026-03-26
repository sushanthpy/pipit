# ──────────────────────────────────────────────────────────────────────
#  _pipit_keybind_send_prompt — Alt+Enter: send current buffer as prompt
# ──────────────────────────────────────────────────────────────────────
function _pipit_keybind_send_prompt
    set -l buf (commandline -b)

    if test -z "$buf"
        commandline -f repaint
        return
    end

    # Clear the command line
    commandline -r ""
    commandline -f repaint

    # Send as prompt
    pipit_send $buf
end
