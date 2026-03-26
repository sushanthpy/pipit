# ──────────────────────────────────────────────────────────────────────
#  _pipit_ensure_conversation — Create conversation ID if missing
# ──────────────────────────────────────────────────────────────────────
function _pipit_ensure_conversation
    if test -z "$pipit_conversation_id"
        # Generate a short random ID (8 hex chars)
        set -U pipit_conversation_id (random)(random | string sub -l 4 | math --base=hex)
        # Normalize to 8-char hex string
        set -U pipit_conversation_id (printf '%08x' (random))
    end
end
