//! Event Taxonomy as a Bounded Distributive Lattice with Bitmask Subscription.
//!
//! Events form a lattice (HookEventMask, ∨, ∧, ⊥, ⊤) where:
//!   ∨ = bitwise OR (join)
//!   ∧ = bitwise AND (meet)
//!   ⊥ = 0 (bottom — no events)
//!   ⊤ = all bits set (top — all events)
//!
//! Subscription match: `subscription ∧ event ≠ ⊥`
//! Dispatch is O(n) with SIMD-amenable inner loop.
//! For expected n ≤ 64 hooks, the constant factor dominates,
//! beating HashMap's cache-unfriendly O(1 + collision).

use serde::{Deserialize, Serialize};

/// Bitmask over the event taxonomy. Each bit is one event type.
/// u64 gives us 64 event slots — sufficient for the foreseeable taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HookEventMask(pub u64);

impl HookEventMask {
    // ── Individual event bits ──
    pub const NONE: Self = Self(0);

    // Session lifecycle
    pub const SESSION_START: Self = Self(1 << 0);
    pub const SESSION_END: Self = Self(1 << 1);

    // Tool lifecycle
    pub const PRE_TOOL_USE: Self = Self(1 << 2);
    pub const TOOL_EXECUTE: Self = Self(1 << 3);
    pub const POST_TOOL_USE: Self = Self(1 << 4);
    pub const POST_TOOL_USE_FAILURE: Self = Self(1 << 5);
    pub const TOOL_APPROVAL_NEEDED: Self = Self(1 << 6);
    pub const TOOL_APPROVAL_RESOLVED: Self = Self(1 << 7);

    // Turn lifecycle
    pub const TURN_START: Self = Self(1 << 8);
    pub const TURN_END: Self = Self(1 << 9);
    pub const TURN_COMMITTED: Self = Self(1 << 10);

    // Content streaming
    pub const CONTENT_DELTA: Self = Self(1 << 11);
    pub const THINKING_DELTA: Self = Self(1 << 12);
    pub const CONTENT_COMPLETE: Self = Self(1 << 13);

    // Planning & verification
    pub const PLAN_SELECTED: Self = Self(1 << 14);
    pub const PLAN_PIVOTED: Self = Self(1 << 15);
    pub const VERIFICATION_START: Self = Self(1 << 16);
    pub const VERIFICATION_VERDICT: Self = Self(1 << 17);
    pub const REPAIR_STARTED: Self = Self(1 << 18);

    // Context management
    pub const PRE_COMPACT: Self = Self(1 << 19);
    pub const COMPACT_EXECUTE: Self = Self(1 << 20);
    pub const POST_COMPACT: Self = Self(1 << 21);

    // Error & control
    pub const PROVIDER_ERROR: Self = Self(1 << 22);
    pub const LOOP_DETECTED: Self = Self(1 << 23);
    pub const STOP: Self = Self(1 << 24);
    pub const CANCELLED: Self = Self(1 << 25);

    // Phase transitions (canonical FSM)
    pub const PHASE_TRANSITION: Self = Self(1 << 26);

    // ── Category masks (join of related events) ──
    pub const SESSION_LIFECYCLE: Self = Self(Self::SESSION_START.0 | Self::SESSION_END.0);

    pub const TOOL_LIFECYCLE: Self = Self(
        Self::PRE_TOOL_USE.0
            | Self::TOOL_EXECUTE.0
            | Self::POST_TOOL_USE.0
            | Self::POST_TOOL_USE_FAILURE.0
            | Self::TOOL_APPROVAL_NEEDED.0
            | Self::TOOL_APPROVAL_RESOLVED.0,
    );

    pub const TURN_LIFECYCLE: Self =
        Self(Self::TURN_START.0 | Self::TURN_END.0 | Self::TURN_COMMITTED.0);

    pub const CONTENT: Self =
        Self(Self::CONTENT_DELTA.0 | Self::THINKING_DELTA.0 | Self::CONTENT_COMPLETE.0);

    pub const PLANNING: Self = Self(
        Self::PLAN_SELECTED.0
            | Self::PLAN_PIVOTED.0
            | Self::VERIFICATION_START.0
            | Self::VERIFICATION_VERDICT.0
            | Self::REPAIR_STARTED.0,
    );

    pub const COMPACTION: Self =
        Self(Self::PRE_COMPACT.0 | Self::COMPACT_EXECUTE.0 | Self::POST_COMPACT.0);

    pub const ERROR: Self = Self(Self::PROVIDER_ERROR.0 | Self::LOOP_DETECTED.0);

    pub const TERMINAL: Self = Self(Self::SESSION_END.0 | Self::STOP.0 | Self::CANCELLED.0);

    /// ⊤ — all events
    pub const ALL: Self = Self(u64::MAX);

    // ── Lattice operations ──

    /// Join (∨ = bitwise OR): subscribe to A or B.
    pub const fn join(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Meet (∧ = bitwise AND): events in both A and B.
    pub const fn meet(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Subscription match: does this mask contain the event?
    /// `self ∧ event ≠ ⊥`
    pub const fn matches(self, event: Self) -> bool {
        (self.0 & event.0) != 0
    }

    /// Number of events in this mask (popcount).
    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }

    /// Is this the bottom element (no events)?
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for HookEventMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.join(rhs)
    }
}

impl std::ops::BitAnd for HookEventMask {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        self.meet(rhs)
    }
}

/// Convert a hook event name string to its bitmask.
pub fn event_name_to_mask(name: &str) -> HookEventMask {
    match name {
        "SessionStart" | "session_start" => HookEventMask::SESSION_START,
        "SessionEnd" | "session_end" => HookEventMask::SESSION_END,
        "PreToolUse" | "pre_tool_use" => HookEventMask::PRE_TOOL_USE,
        "ToolExecute" | "tool_execute" => HookEventMask::TOOL_EXECUTE,
        "PostToolUse" | "post_tool_use" => HookEventMask::POST_TOOL_USE,
        "PostToolUseFailure" | "post_tool_use_failure" => HookEventMask::POST_TOOL_USE_FAILURE,
        "TurnStart" | "turn_start" => HookEventMask::TURN_START,
        "TurnEnd" | "turn_end" => HookEventMask::TURN_END,
        "TurnCommitted" | "turn_committed" => HookEventMask::TURN_COMMITTED,
        "PreCompact" | "pre_compact" => HookEventMask::PRE_COMPACT,
        "CompactExecute" | "compact_execute" => HookEventMask::COMPACT_EXECUTE,
        "PostCompact" | "post_compact" => HookEventMask::POST_COMPACT,
        "Stop" | "stop" => HookEventMask::STOP,
        "PhaseTransition" | "phase_transition" => HookEventMask::PHASE_TRANSITION,
        _ => HookEventMask::NONE,
    }
}

/// Convert a list of event names to a combined subscription mask.
pub fn events_to_mask(names: &[String]) -> HookEventMask {
    names.iter().fold(HookEventMask::NONE, |acc, name| {
        acc | event_name_to_mask(name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_bottom() {
        assert!(HookEventMask::NONE.is_empty());
        assert_eq!(HookEventMask::NONE.count(), 0);
    }

    #[test]
    fn lattice_join_is_union() {
        let a = HookEventMask::PRE_TOOL_USE;
        let b = HookEventMask::POST_TOOL_USE;
        let ab = a | b;
        assert!(ab.matches(a));
        assert!(ab.matches(b));
        assert!(!ab.matches(HookEventMask::SESSION_START));
    }

    #[test]
    fn lattice_meet_is_intersection() {
        let tool = HookEventMask::TOOL_LIFECYCLE;
        let pre = HookEventMask::PRE_TOOL_USE;
        assert!((tool & pre).matches(pre));
        assert!(!(tool & HookEventMask::SESSION_START).matches(HookEventMask::SESSION_START));
    }

    #[test]
    fn category_masks_contain_members() {
        assert!(HookEventMask::TOOL_LIFECYCLE.matches(HookEventMask::PRE_TOOL_USE));
        assert!(HookEventMask::TOOL_LIFECYCLE.matches(HookEventMask::POST_TOOL_USE_FAILURE));
        assert!(!HookEventMask::TOOL_LIFECYCLE.matches(HookEventMask::SESSION_START));
    }

    #[test]
    fn subscription_match() {
        let subscription = HookEventMask::TOOL_LIFECYCLE | HookEventMask::SESSION_LIFECYCLE;
        assert!(subscription.matches(HookEventMask::PRE_TOOL_USE));
        assert!(subscription.matches(HookEventMask::SESSION_END));
        assert!(!subscription.matches(HookEventMask::CONTENT_DELTA));
    }

    #[test]
    fn event_name_parsing() {
        assert_eq!(
            event_name_to_mask("PreToolUse"),
            HookEventMask::PRE_TOOL_USE
        );
        assert_eq!(
            event_name_to_mask("pre_tool_use"),
            HookEventMask::PRE_TOOL_USE
        );
        assert_eq!(event_name_to_mask("unknown"), HookEventMask::NONE);
    }

    #[test]
    fn events_to_mask_combines() {
        let names = vec!["PreToolUse".into(), "PostToolUse".into(), "Stop".into()];
        let mask = events_to_mask(&names);
        assert!(mask.matches(HookEventMask::PRE_TOOL_USE));
        assert!(mask.matches(HookEventMask::POST_TOOL_USE));
        assert!(mask.matches(HookEventMask::STOP));
        assert!(!mask.matches(HookEventMask::SESSION_START));
        assert_eq!(mask.count(), 3);
    }
}
