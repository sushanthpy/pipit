# ──────────────────────────────────────────────────────────────────────
#  pipit_sandbox — Isolated git worktree for safe experimentation
# ──────────────────────────────────────────────────────────────────────
#
#  Fish-native approach:
#    - Universal variables track sandbox state across ALL terminals
#    - `--on-variable` event auto-repaints prompt when sandbox changes
#    - Right prompt shows 🧪 when in sandbox
#
#  Usage:
#    pipit sandbox              → Create worktree, cd into it
#    pipit sandbox exit         → Return to origin, cleanup worktree
#    pipit sandbox status       → Show sandbox info
#    pipit sandbox keep         → Exit but keep the worktree
#
# ──────────────────────────────────────────────────────────────────────
function pipit_sandbox -d "Git worktree sandbox for safe experimentation"
    if test (count $argv) -eq 0
        # Default: create or show status
        if test -n "$pipit_sandbox_worktree"
            pipit_sandbox_status
        else
            _pipit_sandbox_create
        end
        return
    end

    switch $argv[1]
        case exit quit leave
            _pipit_sandbox_exit
        case status
            pipit_sandbox_status
        case keep
            _pipit_sandbox_keep
        case '*'
            _pipit_log error "Unknown sandbox command: $argv[1]"
            echo "  Usage: pipit sandbox [exit|status|keep]"
    end
end

function pipit_sandbox_status -d "Show sandbox status"
    if test -z "$pipit_sandbox_worktree"
        _pipit_log info "Not in a sandbox."
        echo "  Run 'pipit sandbox' to create one."
        return
    end

    echo
    set_color --bold magenta
    echo "  🧪 Sandbox Active"
    set_color normal
    echo

    printf "  %-16s %s\n" "Origin:" $pipit_sandbox_origin
    printf "  %-16s %s\n" "Worktree:" $pipit_sandbox_worktree
    printf "  %-16s %s\n" "Branch:" (git -C $pipit_sandbox_worktree branch --show-current 2>/dev/null; or echo "?")
    printf "  %-16s %s\n" "CWD:" (pwd)

    # Show changes in worktree
    set -l changes (git -C $pipit_sandbox_worktree status --short 2>/dev/null | wc -l | string trim)
    printf "  %-16s %s file(s)\n" "Changes:" $changes
    echo
end

function _pipit_sandbox_create
    if test -n "$pipit_sandbox_worktree"
        _pipit_log warn "Already in a sandbox at: $pipit_sandbox_worktree"
        echo "  Run 'pipit sandbox exit' first."
        return 1
    end

    # Must be in a git repo
    if not git rev-parse --is-inside-work-tree >/dev/null 2>&1
        _pipit_log error "Not inside a git repository."
        return 1
    end

    set -l origin (git rev-parse --show-toplevel)
    set -l branch (git branch --show-current 2>/dev/null; or echo "HEAD")
    set -l sandbox_name "pipit-sandbox-"(date +%Y%m%d-%H%M%S)
    set -l sandbox_branch "sandbox/$sandbox_name"
    set -l worktree_path "$origin/../$sandbox_name"

    # Create worktree with a new branch
    _pipit_log info "Creating sandbox worktree..."
    if not git worktree add -b $sandbox_branch $worktree_path $branch 2>/dev/null
        _pipit_log error "Failed to create worktree."
        return 1
    end

    # Store state in universal variables — persists everywhere
    set -U pipit_sandbox_origin $origin
    set -U pipit_sandbox_worktree (realpath $worktree_path)

    # cd into sandbox
    cd $pipit_sandbox_worktree

    echo
    set_color --bold magenta
    echo "  🧪 Sandbox created"
    set_color normal
    printf "  %-16s %s\n" "Origin:" $pipit_sandbox_origin
    printf "  %-16s %s\n" "Branch:" $sandbox_branch
    printf "  %-16s %s\n" "Worktree:" $pipit_sandbox_worktree
    echo
    _pipit_log info "Safe to experiment. 'pipit sandbox exit' to return and cleanup."
end

function _pipit_sandbox_exit
    if test -z "$pipit_sandbox_worktree"
        _pipit_log error "No active sandbox."
        return 1
    end

    set -l origin $pipit_sandbox_origin
    set -l worktree $pipit_sandbox_worktree

    # Check for uncommitted changes
    set -l dirty (git -C $worktree status --porcelain 2>/dev/null | wc -l | string trim)
    if test "$dirty" != "0"
        _pipit_log warn "$dirty uncommitted change(s) in sandbox."
        read -P (set_color yellow)"  Discard and remove? [y/N] "(set_color normal) -l choice
        if not string match -qi 'y' -- $choice
            _pipit_log info "Keeping sandbox. Use 'pipit sandbox keep' to exit without cleanup."
            return 1
        end
    end

    # Return to origin
    cd $origin

    # Get the branch name before removing
    set -l sandbox_branch (git -C $worktree branch --show-current 2>/dev/null)

    # Remove worktree
    git worktree remove --force $worktree 2>/dev/null
    # Remove the sandbox branch
    if test -n "$sandbox_branch"
        git branch -D $sandbox_branch 2>/dev/null
    end

    # Clear universal state
    set -U pipit_sandbox_origin ""
    set -U pipit_sandbox_worktree ""

    _pipit_log ok "Sandbox removed. Back at: $origin"
end

function _pipit_sandbox_keep
    if test -z "$pipit_sandbox_worktree"
        _pipit_log error "No active sandbox."
        return 1
    end

    set -l worktree $pipit_sandbox_worktree

    # Return to origin but keep the worktree
    cd $pipit_sandbox_origin

    _pipit_log ok "Returned to origin. Worktree kept at: $worktree"
    _pipit_log info "To remove later: git worktree remove $worktree"

    # Clear state
    set -U pipit_sandbox_origin ""
    set -U pipit_sandbox_worktree ""
end
