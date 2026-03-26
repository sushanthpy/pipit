# ──────────────────────────────────────────────────────────────────────
#  pipit_model — Switch or display current model
# ──────────────────────────────────────────────────────────────────────
function pipit_model -d "Set or show the current model"
    if test (count $argv) -eq 0
        if test -n "$pipit_model"
            _pipit_log info "Current model: $pipit_model"
        else
            _pipit_log info "No model override set (using config default)."
        end
        echo "  Usage: pipit model <name>   (set model)"
        echo "         pipit model reset    (clear override)"
        return
    end

    switch $argv[1]
        case reset clear
            set -U pipit_model ""
            _pipit_log ok "Model override cleared — using config default."
        case '*'
            set -U pipit_model $argv[1]
            _pipit_log ok "Model set to: $pipit_model"
    end
end
