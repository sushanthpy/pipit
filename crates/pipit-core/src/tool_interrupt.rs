//! Tool Interrupt and Cancel Semantics
//!
//! Extends the scheduler with cooperative cancellation, interrupt behavior,
//! and speculative prefetch policies. Each tool call gets a cancel token
//! and a defined behavior under user interrupt.

use crate::tool_semantics::{Purity, ToolCategory, builtin_semantics};
use serde::{Deserialize, Serialize};

/// How a tool call should behave under user interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterruptBehavior {
    /// Cancel immediately — safe for read-only operations.
    CancelImmediate,
    /// Let it finish current work unit, then cancel.
    FinishUnit,
    /// Block — must complete or risk corrupt state (e.g., atomic write in progress).
    MustComplete,
    /// Cancel and rollback any partial state.
    CancelAndRollback,
}

/// Whether a tool call can be speculatively prefetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrefetchPolicy {
    /// Safe to prefetch — pure read, no side effects.
    Prefetchable,
    /// Do not prefetch — has side effects or is expensive.
    NoPrefetch,
}

/// Complete scheduling descriptor for a tool call.
#[derive(Debug, Clone)]
pub struct ToolSchedulingDescriptor {
    pub tool_name: String,
    pub purity: Purity,
    pub category: ToolCategory,
    pub interrupt_behavior: InterruptBehavior,
    pub prefetch_policy: PrefetchPolicy,
    /// Whether this call can run concurrently with other calls.
    pub parallelizable: bool,
    /// Estimated duration in seconds.
    pub expected_duration_secs: u32,
}

/// Derive a scheduling descriptor from a tool name.
pub fn scheduling_descriptor(tool_name: &str) -> ToolSchedulingDescriptor {
    let semantics = builtin_semantics(tool_name);

    let interrupt_behavior = match (semantics.purity, semantics.category) {
        (Purity::Pure, _) | (Purity::Idempotent, _) => InterruptBehavior::CancelImmediate,
        (_, ToolCategory::Edit) => InterruptBehavior::MustComplete, // atomic writes
        (_, ToolCategory::Shell) => InterruptBehavior::FinishUnit,
        (Purity::Destructive, _) => InterruptBehavior::CancelAndRollback,
        _ => InterruptBehavior::FinishUnit,
    };

    let prefetch_policy = match semantics.purity {
        Purity::Pure | Purity::Idempotent => PrefetchPolicy::Prefetchable,
        _ => PrefetchPolicy::NoPrefetch,
    };

    let parallelizable = semantics.self_commutative
        && semantics.purity <= Purity::Idempotent;

    ToolSchedulingDescriptor {
        tool_name: tool_name.to_string(),
        purity: semantics.purity,
        category: semantics.category,
        interrupt_behavior,
        prefetch_policy,
        parallelizable,
        expected_duration_secs: semantics.expected_duration_secs,
    }
}

/// Determine whether a pending batch can be cancelled given user interrupt.
pub fn can_cancel_batch(descriptors: &[ToolSchedulingDescriptor]) -> bool {
    descriptors.iter().all(|d| {
        matches!(
            d.interrupt_behavior,
            InterruptBehavior::CancelImmediate | InterruptBehavior::CancelAndRollback
        )
    })
}

/// Identify which calls in a batch should be speculatively prefetched
/// during idle time (e.g., while waiting for user input).
pub fn prefetchable_calls(descriptors: &[ToolSchedulingDescriptor]) -> Vec<&str> {
    descriptors
        .iter()
        .filter(|d| d.prefetch_policy == PrefetchPolicy::Prefetchable)
        .map(|d| d.tool_name.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_tools_are_cancelable() {
        let desc = scheduling_descriptor("read_file");
        assert_eq!(desc.interrupt_behavior, InterruptBehavior::CancelImmediate);
        assert_eq!(desc.prefetch_policy, PrefetchPolicy::Prefetchable);
        assert!(desc.parallelizable);
    }

    #[test]
    fn write_tools_must_complete() {
        let desc = scheduling_descriptor("write_file");
        assert_eq!(desc.interrupt_behavior, InterruptBehavior::MustComplete);
        assert_eq!(desc.prefetch_policy, PrefetchPolicy::NoPrefetch);
        assert!(!desc.parallelizable);
    }

    #[test]
    fn bash_finishes_unit() {
        let desc = scheduling_descriptor("bash");
        assert_eq!(desc.interrupt_behavior, InterruptBehavior::FinishUnit);
        assert!(!desc.parallelizable);
    }

    #[test]
    fn batch_cancel_analysis() {
        let reads = vec![
            scheduling_descriptor("read_file"),
            scheduling_descriptor("grep"),
        ];
        assert!(can_cancel_batch(&reads));

        let mixed = vec![
            scheduling_descriptor("read_file"),
            scheduling_descriptor("write_file"),
        ];
        assert!(!can_cancel_batch(&mixed));
    }
}
