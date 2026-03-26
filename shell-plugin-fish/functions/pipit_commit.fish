# ──────────────────────────────────────────────────────────────────────
#  pipit_commit — AI-generated commit message + git commit
# ──────────────────────────────────────────────────────────────────────
function pipit_commit -d "AI commit message + git commit"
    # Verify we're in a git repo
    if not git rev-parse --is-inside-work-tree >/dev/null 2>&1
        _pipit_log error "Not inside a git repository."
        return 1
    end

    # Check for staged changes
    set -l diff (git diff --cached --stat 2>/dev/null)
    if test -z "$diff"
        # Nothing staged — auto-stage tracked files
        _pipit_log info "No staged changes. Staging all tracked files..."
        git add -u
        set diff (git diff --cached --stat 2>/dev/null)
        if test -z "$diff"
            _pipit_log error "No changes to commit."
            return 1
        end
    end

    echo
    set_color brblack
    echo "  Staged changes:"
    git diff --cached --stat | sed 's/^/    /'
    set_color normal
    echo

    _pipit_log info "Generating commit message..."

    set -l patch (git diff --cached 2>/dev/null)
    set -l system_prompt "You are a git commit message generator. Given a diff, write a concise, conventional commit message. Use the conventional commits format (feat:, fix:, refactor:, docs:, etc.). First line max 72 chars. Add a blank line then bullet points for details if the change is non-trivial. Return ONLY the commit message — no markdown, no backticks."

    set -l message (_pipit_exec prompt --system "$system_prompt" "$patch" 2>/dev/null)

    if test -z "$message"
        _pipit_log error "Failed to generate commit message."
        return 1
    end

    # Strip markdown fencing if present
    set message (string replace -r '^```[a-z]*\n?' '' $message)
    set message (string replace -r '\n?```$' '' $message)

    # Show proposed message
    echo
    set_color --bold green
    echo "  Commit message:"
    set_color normal
    echo $message | sed 's/^/    /'
    echo

    # Prompt: accept / edit / cancel
    read -P (set_color yellow)"  [Y]es / [e]dit / [n]o? "(set_color normal) -l choice

    switch (string lower $choice)
        case '' y yes
            git commit -m "$message"
            _pipit_log ok "Committed."
        case e edit
            # Write message to temp file, open editor
            set -l tmpfile (mktemp /tmp/pipit-commit-XXXXXX)
            echo $message > $tmpfile
            eval $EDITOR $tmpfile
            if test -s $tmpfile
                git commit -F $tmpfile
                _pipit_log ok "Committed with edited message."
            else
                _pipit_log warn "Empty message — commit aborted."
            end
            command rm -f $tmpfile
        case '*'
            _pipit_log info "Commit cancelled."
    end
end
