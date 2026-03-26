# ──────────────────────────────────────────────────────────────────────
#  pipit_conversation — Switch or list conversations
# ──────────────────────────────────────────────────────────────────────
#
#  Usage:
#    pipit conversation           → Show current + pick from list (fzf)
#    pipit conversation -         → Switch to previous (like cd -)
#    pipit conversation <id>      → Switch to specific ID
#
# ──────────────────────────────────────────────────────────────────────
function pipit_conversation -d "Switch or list conversations"
    if test (count $argv) -eq 0
        # Show current, then offer to switch
        _pipit_log info "Current:  $pipit_conversation_id"
        if test -n "$pipit_prev_conversation"
            _pipit_log info "Previous: $pipit_prev_conversation"
        end
        echo
        echo "  pipit conversation -      → switch to previous"
        echo "  pipit conversation <id>   → switch to specific ID"
        return
    end

    switch $argv[1]
        case '-'
            # Swap current ↔ previous (like cd -)
            if test -z "$pipit_prev_conversation"
                _pipit_log error "No previous conversation to switch to."
                return 1
            end
            set -l old $pipit_conversation_id
            set -U pipit_conversation_id $pipit_prev_conversation
            set -U pipit_prev_conversation $old
            _pipit_log ok "Switched to: $pipit_conversation_id (prev: $pipit_prev_conversation)"

        case '*'
            # Direct switch
            set -U pipit_prev_conversation $pipit_conversation_id
            set -U pipit_conversation_id $argv[1]
            _pipit_log ok "Switched to: $pipit_conversation_id"
    end
end
