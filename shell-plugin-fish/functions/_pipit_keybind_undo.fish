# ──────────────────────────────────────────────────────────────────────
#  _pipit_keybind_undo — Ctrl+X,Ctrl+U: undo last file change
# ──────────────────────────────────────────────────────────────────────
function _pipit_keybind_undo
    pipit_undo
    commandline -f repaint
end
