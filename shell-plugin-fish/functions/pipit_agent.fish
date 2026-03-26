# ──────────────────────────────────────────────────────────────────────
#  pipit_agent — Switch or display current agent
# ──────────────────────────────────────────────────────────────────────
function pipit_agent -d "Set or show the active agent"
    if test (count $argv) -eq 0
        if test -n "$pipit_agent"
            _pipit_log info "Current agent: $pipit_agent"
        else
            _pipit_log info "No agent override set (using default)."
        end
        echo "  Usage: pipit agent <name>   (switch agent)"
        echo "         pipit agent reset    (clear override)"
        return
    end

    switch $argv[1]
        case reset clear
            set -U pipit_agent ""
            _pipit_log ok "Agent override cleared."
        case '*'
            set -U pipit_agent $argv[1]
            _pipit_log ok "Agent set to: $pipit_agent"
    end
end
