# ──────────────────────────────────────────────────────────────────────
#  pipit_diff — Colored diff rendering with context
# ──────────────────────────────────────────────────────────────────────
#
#  Fish-native approach:
#    - Uses `string` builtin for line-by-line coloring (no sed/awk)
#    - Grouped chunks with ··· separators
#    - Works with unified diff format from `diff -u`, `git diff`, or files
#
#  Usage:
#    pipit diff file1 file2         → Colored side-by-side diff
#    pipit diff --git               → Colored git diff (staged + unstaged)
#    pipit diff --staged            → Colored git diff (staged only)
#    git diff | pipit diff          → Pipe any diff through colorizer
#
# ──────────────────────────────────────────────────────────────────────
function pipit_diff -d "Colored diff rendering"
    # Determine input mode
    if not isatty stdin
        # Piped input — colorize it
        _pipit_diff_colorize
        return
    end

    if test (count $argv) -eq 0
        # Default: git diff with both staged and unstaged
        git diff --unified=3 | _pipit_diff_colorize
        return
    end

    switch $argv[1]
        case --git -g
            git diff --unified=3 | _pipit_diff_colorize
        case --staged -s
            git diff --cached --unified=3 | _pipit_diff_colorize
        case --help -h
            echo "  Usage:"
            echo "    pipit diff file1 file2    Compare two files"
            echo "    pipit diff --git          Git diff (all changes)"
            echo "    pipit diff --staged       Git diff (staged only)"
            echo "    ... | pipit diff          Colorize piped diff"
        case '*'
            if test (count $argv) -ge 2 -a -f $argv[1] -a -f $argv[2]
                diff -u $argv[1] $argv[2] | _pipit_diff_colorize
            else
                _pipit_log error "Expected two files or a flag."
                echo "  Usage: pipit diff <file1> <file2>"
            end
    end
end

function _pipit_diff_colorize -d "Colorize unified diff from stdin"
    set -l in_chunk 0
    set -l chunk_lines 0

    while read -l line
        # Detect line type using string match
        if string match -qr '^\-\-\-' -- $line
            # File header (old)
            set_color --bold red
            echo $line
            set_color normal
            set in_chunk 0

        else if string match -qr '^\+\+\+' -- $line
            # File header (new)
            set_color --bold green
            echo $line
            set_color normal

        else if string match -qr '^@@' -- $line
            # Chunk header
            if test $in_chunk -eq 1
                # Separator between chunks
                set_color brblack
                echo "  ···"
                set_color normal
            end
            set_color --bold cyan
            echo $line
            set_color normal
            set in_chunk 1
            set chunk_lines 0

        else if string match -qr '^\+' -- $line
            # Added line
            set_color green
            echo $line
            set_color normal
            set chunk_lines (math $chunk_lines + 1)

        else if string match -qr '^\-' -- $line
            # Removed line
            set_color red
            echo $line
            set_color normal
            set chunk_lines (math $chunk_lines + 1)

        else if string match -qr '^diff' -- $line
            # diff header
            echo
            set_color --bold white
            echo $line
            set_color normal
            set in_chunk 0

        else
            # Context line
            set_color brblack
            echo $line
            set_color normal
            set chunk_lines (math $chunk_lines + 1)
        end
    end
end
