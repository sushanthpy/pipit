# ──────────────────────────────────────────────────────────────────────
#  _pipit_log — Structured logging helper
# ──────────────────────────────────────────────────────────────────────
function _pipit_log
    set -l level $argv[1]
    set -l msg $argv[2..-1]

    switch $level
        case error
            set_color red; echo -n "✗ "; set_color normal
            echo $msg
        case warn
            set_color yellow; echo -n "⚠ "; set_color normal
            echo $msg
        case ok
            set_color green; echo -n "✓ "; set_color normal
            echo $msg
        case info
            set_color cyan; echo -n "· "; set_color normal
            echo $msg
        case '*'
            echo $msg
    end
end
