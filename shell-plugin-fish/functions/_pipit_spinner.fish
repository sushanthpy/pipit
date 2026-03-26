# ──────────────────────────────────────────────────────────────────────
#  _pipit_spinner_start / _pipit_spinner_stop — Braille spinner
# ──────────────────────────────────────────────────────────────────────
#
#  Fish-native approach: background job writes spinner frames to stderr,
#  main process does its work, then kills the spinner.
#
#  10-frame Unicode braille animation: ⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏
#  Random status words cycle alongside the spinner.
#
# ──────────────────────────────────────────────────────────────────────

function _pipit_spinner_start -d "Start braille spinner with status label"
    set -l label $argv[1]
    if test -z "$label"
        set label "Working"
    end

    # Don't nest spinners
    if test "$_pipit_spinner_active" = "1"
        return
    end

    set -g _pipit_spinner_active 1
    set -g _pipit_spinner_start_time (date +%s)

    # Run spinner in a background subshell via a temp script
    # Fish doesn't have great inline bg process support, so we use a coproc pattern
    set -l frames ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏
    set -l words "Thinking" "Forging" "Reasoning" "Analyzing" "Computing" "Crafting" "Processing" "Synthesizing"
    set -l tmpscript (mktemp /tmp/pipit-spinner-XXXXXX.fish)

    # Write the spinner script
    echo '
    set frames ⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏
    set words "Thinking" "Forging" "Reasoning" "Analyzing" "Computing" "Crafting" "Processing" "Synthesizing"
    set label "'$label'"
    set i 1
    set w 1
    set start (date +%s)
    while true
        set now (date +%s)
        set elapsed (math $now - $start)
        set frame $frames[$i]
        set word $words[$w]
        printf "\r\033[2K\033[33m  %s \033[0m\033[2m%s · %s · %ds\033[0m" $frame $label $word $elapsed >&2
        set i (math $i % 10 + 1)
        # Rotate word every 3 frames
        if test (math $i % 3) -eq 0
            set w (math $w % (count $words) + 1)
        end
        sleep 0.1
    end
    ' > $tmpscript

    fish $tmpscript &
    set -g _pipit_spinner_pid $last_pid
    set -g _pipit_spinner_script $tmpscript
end

function _pipit_spinner_stop -d "Stop the braille spinner"
    if test "$_pipit_spinner_active" != "1"
        return
    end

    # Kill spinner process
    if test -n "$_pipit_spinner_pid"
        kill $_pipit_spinner_pid 2>/dev/null
        wait $_pipit_spinner_pid 2>/dev/null
    end

    # Clear spinner line
    printf "\r\033[2K" >&2

    # Show elapsed time
    if test -n "$_pipit_spinner_start_time"
        set -l now (date +%s)
        set -l elapsed (math $now - $_pipit_spinner_start_time)
        if test $elapsed -gt 0
            set_color brblack
            echo "  completed in {$elapsed}s" >&2
            set_color normal
        end
    end

    # Cleanup
    set -g _pipit_spinner_active 0
    set -g _pipit_spinner_pid ""
    if set -q _pipit_spinner_script
        command rm -f $_pipit_spinner_script
    end
end
