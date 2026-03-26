# ──────────────────────────────────────────────────────────────────────
#  _pipit_exec — Run pipit binary with session state injected
# ──────────────────────────────────────────────────────────────────────
function _pipit_exec
    if test -z "$_pipit_bin"
        _pipit_log error "pipit binary not found. Set PIPIT_BIN or add pipit to PATH."
        return 1
    end

    # Build argument list with session overrides
    set -l args

    if test -n "$pipit_model"
        set -a args --model $pipit_model
    end
    if test -n "$pipit_provider"
        set -a args --provider $pipit_provider
    end

    # Append caller's arguments
    set -a args $argv

    command $_pipit_bin $args
end
