# ──────────────────────────────────────────────────────────────────────
#  pipit_info — Display current session info in a box
# ──────────────────────────────────────────────────────────────────────
function pipit_info -d "Show pipit session info"
    set -l w 52

    set_color cyan
    echo "╭─── Pipit Session ─────────────────────────────────╮"
    set_color normal

    # Helper: print a row
    function _info_row
        set -l label $argv[1]
        set -l value $argv[2]
        if test -z "$value"
            set value (set_color brblack)"(not set)"(set_color normal)
        end
        printf "│  %-16s %s\n" "$label" "$value"
    end

    _info_row "conversation" $pipit_conversation_id
    _info_row "prev conv" $pipit_prev_conversation
    _info_row "model" $pipit_model
    _info_row "provider" $pipit_provider
    _info_row "agent" $pipit_agent
    _info_row "pipit binary" $_pipit_bin
    _info_row "fzf" (test -n "$_pipit_has_fzf"; and echo "yes"; or echo "no")
    _info_row "fd" (test -n "$_pipit_has_fd"; and echo "yes"; or echo "no")
    _info_row "bat" (test -n "$_pipit_has_bat"; and echo "yes"; or echo "no")
    _info_row "cwd" (prompt_pwd)

    set_color cyan
    echo "╰──────────────────────────────────────────────────╯"
    set_color normal

    # Clean up helper
    functions -e _info_row
end
