# ──────────────────────────────────────────────────────────────────────
#  _pipit_keybind_model_picker — Ctrl+X,Ctrl+M: fzf model selector
# ──────────────────────────────────────────────────────────────────────
#
#  Shows available models via fzf with provider info.
#  Selected model is set as session override (universal var).
#
# ──────────────────────────────────────────────────────────────────────
function _pipit_keybind_model_picker
    if test -z "$_pipit_has_fzf"
        commandline -f repaint
        return
    end

    # Model catalog — curated list with provider and tier info
    # Format: "model_id | provider | tier | context"
    set -l models \
        "claude-sonnet-4-20250514|anthropic|flagship|200K" \
        "claude-3-5-haiku-20241022|anthropic|fast|200K" \
        "gpt-4o|openai|flagship|128K" \
        "gpt-4o-mini|openai|fast|128K" \
        "o3-mini|openai|reasoning|128K" \
        "gemini-2.0-flash|google|fast|1M" \
        "gemini-2.5-pro-preview-05-06|google|flagship|1M" \
        "deepseek-chat|deepseek|flagship|64K" \
        "deepseek-reasoner|deepseek|reasoning|64K" \
        "grok-3|xai|flagship|131K" \
        "grok-3-mini|xai|fast|131K" \
        "mistral-large-latest|mistral|flagship|128K" \
        "llama-3.3-70b|groq|fast|128K" \
        "llama-3.1-8b|cerebras|instant|128K" \
        "qwen/qwen-2.5-72b-instruct|openrouter|flagship|128K"

    # Build display lines for fzf
    set -l display_lines
    for m in $models
        set -l parts (string split '|' -- $m)
        set -l model_id $parts[1]
        set -l provider $parts[2]
        set -l tier $parts[3]
        set -l ctx $parts[4]
        set -a display_lines (printf "%-42s  %-12s  %-10s  %s" $model_id $provider $tier $ctx)
    end

    set -l header (printf "%-42s  %-12s  %-10s  %s" "MODEL" "PROVIDER" "TIER" "CONTEXT")

    set -l picked (printf '%s\n' $display_lines | \
        _pipit_fzf --prompt "Model> " --header "$header" \
            --preview "echo 'Set this as your active model'" \
            --preview-window=hidden)

    if test -z "$picked"
        commandline -f repaint
        return
    end

    # Extract model ID (first whitespace-delimited word)
    set -l model_id (string trim -- (string split -m1 ' ' -- $picked)[1])

    # Extract provider
    set -l provider ""
    for m in $models
        if string match -q "$model_id*" -- $m
            set provider (string split '|' -- $m)[2]
            break
        end
    end

    # Set both model and provider as universal vars
    set -U pipit_model $model_id
    if test -n "$provider"
        set -U pipit_provider $provider
    end

    _pipit_log ok "Model: $model_id ($provider)"
    commandline -f repaint
end
