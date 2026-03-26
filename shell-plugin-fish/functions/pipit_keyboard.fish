# ──────────────────────────────────────────────────────────────────────
#  pipit_keyboard — Show key bindings
# ──────────────────────────────────────────────────────────────────────
function pipit_keyboard -d "Show pipit key bindings"
    echo
    set_color --bold cyan
    echo "  Pipit Key Bindings"
    set_color normal
    echo

    printf "    %-20s %s\n" "Ctrl+F"          "AI suggest from current command line"
    printf "    %-20s %s\n" "Ctrl+X, Ctrl+F"  "Fuzzy file picker → insert @[path]"
    printf "    %-20s %s\n" "Ctrl+X, Ctrl+M"  "Model selector (fzf) → set model"
    printf "    %-20s %s\n" "Ctrl+X, Ctrl+U"  "Undo last file change"
    printf "    %-20s %s\n" "Alt+Enter"       "Send current line as prompt to pipit"
    echo

    set_color brblack
    echo "  Fish native bindings: Tab (complete), → (accept suggestion), ↑ (history)"
    set_color normal
    echo
end
