# ──────────────────────────────────────────────────────────────────────
#  pipit — Main command dispatcher
# ──────────────────────────────────────────────────────────────────────
#
#  Usage:
#    pipit <subcommand> [args...]
#    pipit prompt text here        (sends to active agent)
#
#  Fish autoloads this function on first invocation of `pipit`.
#  Subcommands are dispatched to individual pipit_<cmd>.fish functions.
#
# ──────────────────────────────────────────────────────────────────────
function pipit -d "AI-augmented shell — pipit CLI integration"
    if test (count $argv) -eq 0
        pipit_help
        return
    end

    set -l cmd $argv[1]
    set -l rest $argv[2..-1]

    switch $cmd
        # ── Core ──
        case new n
            pipit_new $rest
        case info i
            pipit_info
        case env e
            pipit_env
        case help h
            pipit_help
        case keyboard
            pipit_keyboard

        # ── AI actions ──
        case suggest s
            pipit_suggest $rest
        case commit
            pipit_commit $rest
        case commit-preview
            pipit_commit_preview
        case agent a
            pipit_agent $rest

        # ── Config ──
        case model
            pipit_model $rest
        case provider
            pipit_provider $rest

        # ── Conversations ──
        case conversation c conv
            pipit_conversation $rest

        # ── Auth ──
        case auth
            pipit_auth $rest

        # ── Sandbox (git worktree isolation) ──
        case sandbox
            pipit_sandbox $rest

        # ── Undo (file change tracking) ──
        case undo
            pipit_undo $rest

        # ── Todo (task tracking) ──
        case todo t
            pipit_todo $rest

        # ── Data gen (JSONL batch) ──
        case data
            pipit_data $rest

        # ── Diff rendering ──
        case diff
            pipit_diff $rest

        # ── Diagnostics ──
        case doctor doc
            pipit_doctor

        # ── Copy last response ──
        case copy cp
            pipit_copy

        # ── Default: treat as prompt to send ──
        case '*'
            pipit_send $argv
    end
end
