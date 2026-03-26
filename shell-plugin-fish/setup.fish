# ──────────────────────────────────────────────────────────────────────
#  setup.fish — Install the Pipit Fish shell plugin
# ──────────────────────────────────────────────────────────────────────
#
#  Usage:
#    fish shell-plugin-fish/setup.fish
#
#  This creates symlinks in Fish's standard directories:
#    ~/.config/fish/conf.d/pipit.fish    → auto-loaded config
#    ~/.config/fish/functions/pipit*.fish → auto-loaded functions
#    ~/.config/fish/completions/pipit.fish → auto-loaded completions
#
# ──────────────────────────────────────────────────────────────────────
function pipit_setup -d "Install Pipit Fish shell plugin"
    set -l plugin_dir (status dirname)
    if not test -d "$plugin_dir/conf.d"
        # If called as `fish setup.fish`, resolve relative to the script
        set plugin_dir (realpath (status filename | path dirname)/.)
    end

    set -l fish_conf "$HOME/.config/fish"

    # Ensure Fish config directories exist
    mkdir -p "$fish_conf/conf.d" "$fish_conf/functions" "$fish_conf/completions"

    echo
    set_color --bold cyan
    echo "  Pipit Fish Plugin — Setup"
    set_color normal
    echo

    # 1. Symlink conf.d entry
    set -l target "$fish_conf/conf.d/pipit.fish"
    if test -e $target
        set_color yellow; echo "  ⚠ $target already exists — skipping"; set_color normal
    else
        ln -s "$plugin_dir/conf.d/pipit.fish" $target
        set_color green; echo "  ✓ Linked conf.d/pipit.fish"; set_color normal
    end

    # 2. Symlink all functions
    for func in $plugin_dir/functions/*.fish
        set -l name (path basename $func)
        set -l target "$fish_conf/functions/$name"
        if test -e $target
            set_color yellow; echo "  ⚠ functions/$name already exists — skipping"; set_color normal
        else
            ln -s $func $target
            set_color green; echo "  ✓ Linked functions/$name"; set_color normal
        end
    end

    # 3. Symlink completions
    set -l target "$fish_conf/completions/pipit.fish"
    if test -e $target
        set_color yellow; echo "  ⚠ completions/pipit.fish already exists — skipping"; set_color normal
    else
        ln -s "$plugin_dir/completions/pipit.fish" $target
        set_color green; echo "  ✓ Linked completions/pipit.fish"; set_color normal
    end

    echo
    set_color --bold
    echo "  Done! Restart your fish shell or run: exec fish"
    set_color normal
    echo
end

# Auto-run if sourced directly (not as a function)
if status is-interactive
    pipit_setup
else
    pipit_setup
end
