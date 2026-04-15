//! T5: Spinner personality verbs — whimsical phase-aware labels.
//!
//! Instead of a static "Thinking…", the spinner cycles through verbs
//! that match the current PEV phase. Each phase has its own pool of
//! verbs; verbs rotate every few seconds so the UI feels alive.

use std::time::{Duration, Instant};

/// The PEV phase the agent is currently in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentPhase {
    Plan,
    Execute,
    Verify,
    Repair,
    Idle,
}

/// Manages rotating spinner verbs, one per phase.
#[derive(Debug, Clone)]
pub struct SpinnerVerbs {
    phase: AgentPhase,
    index: usize,
    last_rotate: Instant,
    rotate_interval: Duration,
}

impl Default for SpinnerVerbs {
    fn default() -> Self {
        Self {
            phase: AgentPhase::Idle,
            index: 0,
            last_rotate: Instant::now(),
            rotate_interval: Duration::from_secs(3),
        }
    }
}

impl SpinnerVerbs {
    pub fn new(phase: AgentPhase) -> Self {
        Self {
            phase,
            ..Default::default()
        }
    }

    /// Update the phase; resets verb rotation when phase changes.
    pub fn set_phase(&mut self, phase: AgentPhase) {
        if self.phase != phase {
            self.phase = phase;
            self.index = 0;
            self.last_rotate = Instant::now();
        }
    }

    /// Advance the verb rotation if enough time has passed.
    /// Call this each render tick.
    pub fn tick(&mut self) {
        if self.last_rotate.elapsed() >= self.rotate_interval {
            let pool = self.pool();
            if !pool.is_empty() {
                self.index = (self.index + 1) % pool.len();
            }
            self.last_rotate = Instant::now();
        }
    }

    /// Current verb for the active phase.
    pub fn current(&self) -> &'static str {
        let pool = self.pool();
        if pool.is_empty() {
            "Working…"
        } else {
            pool[self.index % pool.len()]
        }
    }

    /// Verb pool for the current phase.
    fn pool(&self) -> &'static [&'static str] {
        match self.phase {
            AgentPhase::Plan => PLAN_VERBS,
            AgentPhase::Execute => EXECUTE_VERBS,
            AgentPhase::Verify => VERIFY_VERBS,
            AgentPhase::Repair => REPAIR_VERBS,
            AgentPhase::Idle => IDLE_VERBS,
        }
    }

    /// Get the current phase.
    pub fn phase(&self) -> AgentPhase {
        self.phase
    }

    /// Set rotation interval.
    pub fn rotate_every(mut self, dur: Duration) -> Self {
        self.rotate_interval = dur;
        self
    }
}

// ── Verb pools ────────────────────────────────────────────────────────

static PLAN_VERBS: &[&str] = &[
    "Sketching a plan…",
    "Charting the course…",
    "Mapping dependencies…",
    "Strategizing…",
    "Decomposing the problem…",
    "Weighing options…",
    "Drafting an approach…",
    "Analyzing requirements…",
    "Gathering context…",
    "Forming a hypothesis…",
    "Deliberating…",
    "Consulting the codebase…",
    "Reading the terrain…",
    "Surveying the landscape…",
    "Outlining steps…",
    "Brainstorming…",
    "Picking a strategy…",
    "Evaluating trade-offs…",
    "Thinking it through…",
    "Considering angles…",
];

static EXECUTE_VERBS: &[&str] = &[
    "Coding…",
    "Writing code…",
    "Editing files…",
    "Crafting a solution…",
    "Building…",
    "Implementing…",
    "Wiring things up…",
    "Patching…",
    "Stitching modules…",
    "Generating output…",
    "Applying changes…",
    "Typing furiously…",
    "Sculpting code…",
    "Assembling pieces…",
    "Refactoring…",
    "Constructing…",
    "Compiling thoughts…",
    "Hammering out logic…",
    "Laying down lines…",
    "Making it happen…",
];

static VERIFY_VERBS: &[&str] = &[
    "Verifying…",
    "Running checks…",
    "Testing…",
    "Validating output…",
    "Inspecting results…",
    "Double-checking…",
    "Reviewing changes…",
    "Sanity-checking…",
    "Cross-referencing…",
    "Confirming correctness…",
    "Checking invariants…",
    "Eyeballing the diff…",
    "Auditing…",
    "Probing edge cases…",
    "Stress-testing…",
    "Scanning for issues…",
    "Comparing against spec…",
    "Ensuring quality…",
    "Running diagnostics…",
    "Certifying output…",
];

static REPAIR_VERBS: &[&str] = &[
    "Fixing…",
    "Repairing…",
    "Patching the bug…",
    "Correcting course…",
    "Adjusting…",
    "Healing the code…",
    "Applying a bandage…",
    "Resolving the issue…",
    "Smoothing things over…",
    "Rethinking…",
    "Trying another angle…",
    "Debugging…",
    "Untangling…",
    "Recovering…",
    "Iterating on the fix…",
    "Rewiring…",
    "Compensating…",
    "Adapting…",
    "Mending…",
    "Retrying with tweaks…",
];

static IDLE_VERBS: &[&str] = &[
    "Waiting…",
    "Standing by…",
    "Ready…",
    "Listening…",
    "On standby…",
    "Awaiting input…",
    "Pondering…",
    "Meditating…",
    "Resting…",
    "Idling…",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_idle() {
        let sv = SpinnerVerbs::default();
        assert_eq!(sv.phase(), AgentPhase::Idle);
    }

    #[test]
    fn phase_change_resets_index() {
        let mut sv = SpinnerVerbs::default();
        sv.index = 5;
        sv.set_phase(AgentPhase::Plan);
        assert_eq!(sv.index, 0);
        assert_eq!(sv.phase(), AgentPhase::Plan);
    }

    #[test]
    fn current_returns_valid_verb() {
        for phase in &[AgentPhase::Plan, AgentPhase::Execute, AgentPhase::Verify, AgentPhase::Repair, AgentPhase::Idle] {
            let sv = SpinnerVerbs::new(*phase);
            let verb = sv.current();
            assert!(verb.ends_with('…'), "verb should end with ellipsis: {verb}");
        }
    }

    #[test]
    fn each_pool_has_entries() {
        assert!(!PLAN_VERBS.is_empty());
        assert!(!EXECUTE_VERBS.is_empty());
        assert!(!VERIFY_VERBS.is_empty());
        assert!(!REPAIR_VERBS.is_empty());
        assert!(!IDLE_VERBS.is_empty());
    }

    #[test]
    fn manual_advance_cycles() {
        let mut sv = SpinnerVerbs::new(AgentPhase::Plan);
        let first = sv.current();
        sv.index = 1;
        let second = sv.current();
        assert_ne!(first, second);
    }
}
