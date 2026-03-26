# ──────────────────────────────────────────────────────────────────────
#  _pipit_fzf — Run fzf with consistent styling
# ──────────────────────────────────────────────────────────────────────
function _pipit_fzf
    if test -z "$_pipit_has_fzf"
        _pipit_log error "fzf not found — install it for fuzzy selection."
        return 1
    end

    fzf --height=40% --reverse --border=rounded \
        --color='hl:yellow,hl+:yellow:bold,info:cyan,prompt:cyan' \
        $argv
end
