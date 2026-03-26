# ──────────────────────────────────────────────────────────────────────
#  pipit_new — Start a new conversation
# ──────────────────────────────────────────────────────────────────────
function pipit_new -d "Start a new pipit conversation"
    # Save current conversation as previous
    if test -n "$pipit_conversation_id"
        set -U pipit_prev_conversation $pipit_conversation_id
    end

    # Generate new conversation ID
    set -U pipit_conversation_id (printf '%08x' (random))

    # Optionally switch agent
    if test (count $argv) -gt 0
        set -U pipit_agent $argv[1]
        _pipit_log ok "New conversation with agent: $pipit_agent"
    else
        _pipit_log ok "New conversation: $pipit_conversation_id"
    end
end
