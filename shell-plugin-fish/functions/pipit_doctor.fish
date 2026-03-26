# ──────────────────────────────────────────────────────────────────────
#  pipit_doctor — Run diagnostic checks
# ──────────────────────────────────────────────────────────────────────
function pipit_doctor -d "Run pipit plugin diagnostics"
    echo
    set_color cyan
    echo "  ╭─── Pipit Doctor ────────────────────────────────────╮"
    set_color normal

    set -l pass 0
    set -l fail 0

    # Helper: check a binary
    function _doc_check
        set -l label $argv[1]
        set -l cmd $argv[2]
        if command -sq $cmd
            set -l ver (command $cmd --version 2>&1 | head -1)
            set_color green; printf "  │  ✓ %-18s %s\n" $label $ver; set_color normal
            set pass (math $pass + 1)
        else
            set_color red; printf "  │  ✗ %-18s not found\n" $label; set_color normal
            set fail (math $fail + 1)
        end
    end

    # Core binary
    if test -n "$_pipit_bin" -a (command -sq $_pipit_bin; and echo 1; or echo 0) = 1
        set -l ver (command $_pipit_bin --version 2>&1 | head -1)
        set_color green; printf "  │  ✓ %-18s %s\n" "pipit binary" $ver; set_color normal
        set pass (math $pass + 1)
    else
        set_color red; printf "  │  ✗ %-18s not found\n" "pipit binary"; set_color normal
        set fail (math $fail + 1)
    end

    _doc_check "git" git
    _doc_check "fzf" fzf

    # fd (might be fdfind on Debian/Ubuntu)
    if command -sq fd
        set -l ver (fd --version 2>&1 | head -1)
        set_color green; printf "  │  ✓ %-18s %s\n" "fd" $ver; set_color normal
        set pass (math $pass + 1)
    else if command -sq fdfind
        set_color green; printf "  │  ✓ %-18s %s (as fdfind)\n" "fd" (fdfind --version 2>&1 | head -1); set_color normal
        set pass (math $pass + 1)
    else
        set_color brblack; printf "  │  · %-18s not found (optional)\n" "fd"; set_color normal
    end

    # bat (might be batcat)
    if command -sq bat
        set -l ver (bat --version 2>&1 | head -1)
        set_color green; printf "  │  ✓ %-18s %s\n" "bat" $ver; set_color normal
        set pass (math $pass + 1)
    else if command -sq batcat
        set_color green; printf "  │  ✓ %-18s %s (as batcat)\n" "bat" (batcat --version 2>&1 | head -1); set_color normal
        set pass (math $pass + 1)
    else
        set_color brblack; printf "  │  · %-18s not found (optional)\n" "bat"; set_color normal
    end

    # Editor
    set -l editor (test -n "$EDITOR"; and echo $EDITOR; or echo "(not set)")
    set_color cyan; printf "  │  · %-18s %s\n" "editor" $editor; set_color normal

    # Credentials file
    set -l cred_file "$HOME/.pipit/credentials.json"
    if test -f $cred_file
        set -l count (cat $cred_file | string match -rc '"[^"]+":' | count)
        set_color green; printf "  │  ✓ %-18s %d provider(s) stored\n" "credentials" $count; set_color normal
        set pass (math $pass + 1)
    else
        set_color brblack; printf "  │  · %-18s no credentials file\n" "credentials"; set_color normal
    end

    # API keys in environment
    set -l key_count 0
    for key in ANTHROPIC_API_KEY OPENAI_API_KEY DEEPSEEK_API_KEY GOOGLE_API_KEY OPENROUTER_API_KEY XAI_API_KEY CEREBRAS_API_KEY GROQ_API_KEY MISTRAL_API_KEY
        if set -q $key
            set key_count (math $key_count + 1)
        end
    end
    if test $key_count -gt 0
        set_color green; printf "  │  ✓ %-18s %d API key(s) set\n" "env keys" $key_count; set_color normal
    else
        set_color brblack; printf "  │  · %-18s no API keys in env\n" "env keys"; set_color normal
    end

    # Universal variables (persistent state)
    set_color cyan; printf "  │  · %-18s %s\n" "conversation" (test -n "$pipit_conversation_id"; and echo $pipit_conversation_id; or echo "(none)"); set_color normal
    set_color cyan; printf "  │  · %-18s %s\n" "model" (test -n "$pipit_model"; and echo $pipit_model; or echo "(default)"); set_color normal
    set_color cyan; printf "  │  · %-18s %s\n" "provider" (test -n "$pipit_provider"; and echo $pipit_provider; or echo "(default)"); set_color normal

    # Plugin status
    if set -q _pipit_plugin_loaded
        set_color green; printf "  │  ✓ %-18s loaded\n" "plugin"; set_color normal
        set pass (math $pass + 1)
    else
        set_color red; printf "  │  ✗ %-18s not loaded\n" "plugin"; set_color normal
        set fail (math $fail + 1)
    end

    echo "  │"
    if test $fail -eq 0
        set_color green
        echo "  │  All checks passed."
    else
        set_color yellow
        echo "  │  $fail issue(s) found."
    end
    set_color cyan
    echo "  ╰──────────────────────────────────────────────────────╯"
    set_color normal
    echo

    # Clean up helper
    functions -e _doc_check
end
