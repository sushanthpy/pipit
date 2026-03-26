# ──────────────────────────────────────────────────────────────────────
#  pipit_provider — Switch or display current provider
# ──────────────────────────────────────────────────────────────────────
function pipit_provider -d "Set or show the current provider"
    if test (count $argv) -eq 0
        if test -n "$pipit_provider"
            _pipit_log info "Current provider: $pipit_provider"
        else
            _pipit_log info "No provider override set (using config default)."
        end
        echo "  Usage: pipit provider <name>   (set provider)"
        echo "         pipit provider reset    (clear override)"
        echo
        echo "  Available: anthropic, openai, deepseek, google, openrouter,"
        echo "             xai, cerebras, groq, mistral, ollama"
        return
    end

    switch $argv[1]
        case reset clear
            set -U pipit_provider ""
            _pipit_log ok "Provider override cleared — using config default."
        case anthropic openai deepseek google openrouter xai cerebras groq mistral ollama
            set -U pipit_provider $argv[1]
            _pipit_log ok "Provider set to: $pipit_provider"
        case '*'
            _pipit_log error "Unknown provider: $argv[1]"
            echo "  Available: anthropic, openai, deepseek, google, openrouter, xai, cerebras, groq, mistral, ollama"
    end
end
