# ──────────────────────────────────────────────────────────────────────
#  pipit_env — Show environment variables and API keys (masked)
# ──────────────────────────────────────────────────────────────────────
function pipit_env -d "Show pipit environment and API keys"
    echo
    set_color --bold cyan
    echo "  Environment"
    set_color normal
    echo

    # API key env vars to check
    set -l keys \
        ANTHROPIC_API_KEY \
        OPENAI_API_KEY \
        DEEPSEEK_API_KEY \
        GOOGLE_API_KEY \
        OPENROUTER_API_KEY \
        XAI_API_KEY \
        CEREBRAS_API_KEY \
        GROQ_API_KEY \
        MISTRAL_API_KEY \
        PIPIT_API_KEY

    for key in $keys
        set -l val (eval echo \$$key 2>/dev/null)
        if test -n "$val"
            # Mask: show first 4 and last 4 chars
            set -l len (string length $val)
            if test $len -gt 8
                set -l masked (string sub -l 4 $val)"····"(string sub -s (math $len - 3) $val)
            else
                set -l masked "····"
            end
            set_color green; printf "  ✓ %-24s %s\n" $key $masked; set_color normal
        else
            set_color brblack; printf "  · %-24s (not set)\n" $key; set_color normal
        end
    end

    echo
    printf "  %-24s %s\n" "PIPIT_BIN" (test -n "$_pipit_bin"; and echo $_pipit_bin; or echo "(not set)")
    printf "  %-24s %s\n" "PIPIT_BASE_URL" (test -n "$PIPIT_BASE_URL"; and echo $PIPIT_BASE_URL; or echo "(not set)")
    echo
end
