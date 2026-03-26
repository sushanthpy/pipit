# ──────────────────────────────────────────────────────────────────────
#  pipit_commit_preview — Preview AI commit message without committing
# ──────────────────────────────────────────────────────────────────────
function pipit_commit_preview -d "Preview AI-generated commit message"
    if not git rev-parse --is-inside-work-tree >/dev/null 2>&1
        _pipit_log error "Not inside a git repository."
        return 1
    end

    set -l diff (git diff --cached --stat 2>/dev/null)
    if test -z "$diff"
        set diff (git diff --stat 2>/dev/null)
        if test -z "$diff"
            _pipit_log error "No changes found."
            return 1
        end
        _pipit_log info "Showing preview for unstaged changes:"
        set -l patch (git diff 2>/dev/null)
    else
        set -l patch (git diff --cached 2>/dev/null)
    end

    set -l patch (git diff --cached 2>/dev/null; or git diff 2>/dev/null)
    set -l system_prompt "You are a git commit message generator. Given a diff, write a concise, conventional commit message. Use the conventional commits format (feat:, fix:, refactor:, docs:, etc.). First line max 72 chars. Return ONLY the commit message — no markdown, no backticks."

    set -l message (_pipit_exec prompt --system "$system_prompt" "$patch" 2>/dev/null)

    if test -n "$message"
        set message (string replace -r '^```[a-z]*\n?' '' $message)
        set message (string replace -r '\n?```$' '' $message)
        echo
        set_color --bold cyan
        echo "  Preview commit message:"
        set_color normal
        echo $message | sed 's/^/    /'
        echo
    else
        _pipit_log error "Failed to generate commit message."
    end
end
