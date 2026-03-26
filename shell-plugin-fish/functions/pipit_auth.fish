# ──────────────────────────────────────────────────────────────────────
#  pipit_auth — Credential management (delegates to pipit CLI)
# ──────────────────────────────────────────────────────────────────────
function pipit_auth -d "Manage pipit credentials"
    if test (count $argv) -eq 0
        # Default: show status
        _pipit_exec auth status
        return
    end

    switch $argv[1]
        case status
            _pipit_exec auth status
        case login
            if test (count $argv) -lt 2
                _pipit_log error "Usage: pipit auth login <provider> [--api-key KEY] [--device] [--adc]"
                return 1
            end
            _pipit_exec auth login $argv[2..-1]
        case logout
            if test (count $argv) -lt 2
                _pipit_log error "Usage: pipit auth logout <provider>"
                return 1
            end
            _pipit_exec auth logout $argv[2..-1]
        case '*'
            _pipit_log error "Unknown auth subcommand: $argv[1]"
            echo "  Usage: pipit auth status|login|logout"
    end
end
