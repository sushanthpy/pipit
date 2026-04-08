//! Synthetic User Behavior Model — Task 7.1
//!
//! User behavior as Markov Decision Process (MDP).
//! Each archetype has (S, A, T, R): states, actions, transition probs, rewards.
//! Action selection: Boltzmann distribution P(a|s) = exp(Q(s,a)/τ) / Σ exp(Q(s,a')/τ).
//! Session generation: O(L) per session, O(N·L) for N concurrent users.

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// User archetype defining behavioral patterns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserArchetype {
    pub name: String,
    pub states: Vec<String>,
    /// Transition matrix: transitions[from_state][action] = Vec<(to_state_idx, probability)>
    pub transitions: HashMap<String, HashMap<String, Vec<(usize, f64)>>>,
    /// Q-values: q_values[state][action] = expected utility
    pub q_values: HashMap<String, HashMap<String, f64>>,
    /// Temperature: low τ = deterministic, high τ = random exploration
    pub temperature: f64,
    /// Weight in the population (fraction of total users)
    pub population_weight: f64,
}

/// A single user session: sequence of (state, action) pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSession {
    pub archetype: String,
    pub steps: Vec<SessionStep>,
    pub outcome: SessionOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStep {
    pub state: String,
    pub action: String,
    pub timestamp_offset_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SessionOutcome {
    Completed,
    Abandoned,
    Error,
}

/// A universe of synthetic users generating interaction streams.
pub struct UserUniverse {
    pub archetypes: Vec<UserArchetype>,
}

impl UserUniverse {
    pub fn new(archetypes: Vec<UserArchetype>) -> Self {
        Self { archetypes }
    }

    /// Create a default universe with standard archetypes.
    pub fn default_archetypes() -> Self {
        Self {
            archetypes: vec![
                Self::power_user(),
                Self::casual_user(),
                Self::confused_user(),
                Self::adversarial_user(),
            ],
        }
    }

    /// Generate N user sessions.
    pub fn generate_sessions(&self, n: usize, max_steps: usize) -> Vec<UserSession> {
        let mut rng = rand::thread_rng();
        let mut sessions = Vec::with_capacity(n);

        for _ in 0..n {
            // Select archetype by population weight
            let archetype = self.select_archetype(&mut rng);
            let session = self.generate_session(archetype, max_steps, &mut rng);
            sessions.push(session);
        }

        sessions
    }

    fn select_archetype<R: Rng>(&self, rng: &mut R) -> &UserArchetype {
        let total_weight: f64 = self.archetypes.iter().map(|a| a.population_weight).sum();
        let mut r = rng.gen_range(0.0..1.0) * total_weight;
        for arch in &self.archetypes {
            r -= arch.population_weight;
            if r <= 0.0 {
                return arch;
            }
        }
        self.archetypes.last().unwrap()
    }

    fn generate_session<R: Rng>(
        &self,
        archetype: &UserArchetype,
        max_steps: usize,
        rng: &mut R,
    ) -> UserSession {
        let mut steps = Vec::new();
        let mut current_state = archetype.states.first().cloned().unwrap_or_default();
        let mut timestamp = 0u64;

        for _ in 0..max_steps {
            let action = select_action(archetype, &current_state, rng);
            steps.push(SessionStep {
                state: current_state.clone(),
                action: action.clone(),
                timestamp_offset_ms: timestamp,
            });

            // Transition
            if let Some(next) = transition(archetype, &current_state, &action, rng) {
                current_state = next;
            } else {
                break; // Terminal state
            }

            timestamp += rng.gen_range(500..5000); // 0.5s to 5s between actions

            if current_state == "done" || current_state == "exit" {
                break;
            }
        }

        let outcome = if current_state == "done" || current_state == "checkout_complete" {
            SessionOutcome::Completed
        } else if current_state == "error" {
            SessionOutcome::Error
        } else {
            SessionOutcome::Abandoned
        };

        UserSession {
            archetype: archetype.name.clone(),
            steps,
            outcome,
        }
    }

    // ── Default archetypes ──

    fn power_user() -> UserArchetype {
        let states = vec!["home", "search", "product", "cart", "checkout", "done"]
            .into_iter()
            .map(String::from)
            .collect();
        let mut q = HashMap::new();
        q.insert(
            "home".into(),
            HashMap::from([("search".into(), 0.8), ("browse".into(), 0.5)]),
        );
        q.insert(
            "search".into(),
            HashMap::from([("select".into(), 0.9), ("refine".into(), 0.6)]),
        );
        q.insert(
            "product".into(),
            HashMap::from([("add_cart".into(), 0.85), ("back".into(), 0.2)]),
        );
        q.insert(
            "cart".into(),
            HashMap::from([("checkout".into(), 0.9), ("remove".into(), 0.1)]),
        );
        q.insert(
            "checkout".into(),
            HashMap::from([("pay".into(), 0.95), ("abandon".into(), 0.05)]),
        );

        UserArchetype {
            name: "power_user".into(),
            states,
            transitions: default_transitions(),
            q_values: q,
            temperature: 0.5, // Mostly deterministic
            population_weight: 0.3,
        }
    }

    fn casual_user() -> UserArchetype {
        let states = vec!["home", "browse", "product", "cart", "checkout", "done"]
            .into_iter()
            .map(String::from)
            .collect();
        let mut q = HashMap::new();
        q.insert(
            "home".into(),
            HashMap::from([("browse".into(), 0.7), ("search".into(), 0.3)]),
        );
        q.insert(
            "browse".into(),
            HashMap::from([("select".into(), 0.5), ("browse".into(), 0.4)]),
        );
        q.insert(
            "product".into(),
            HashMap::from([("add_cart".into(), 0.4), ("back".into(), 0.6)]),
        );
        q.insert(
            "cart".into(),
            HashMap::from([("checkout".into(), 0.5), ("abandon".into(), 0.4)]),
        );

        UserArchetype {
            name: "casual_user".into(),
            states,
            transitions: default_transitions(),
            q_values: q,
            temperature: 1.5, // More random
            population_weight: 0.4,
        }
    }

    fn confused_user() -> UserArchetype {
        let states = vec!["home", "search", "product", "help", "cart", "done"]
            .into_iter()
            .map(String::from)
            .collect();
        let mut q = HashMap::new();
        q.insert(
            "home".into(),
            HashMap::from([("search".into(), 0.3), ("help".into(), 0.7)]),
        );
        q.insert(
            "search".into(),
            HashMap::from([("back".into(), 0.5), ("select".into(), 0.3)]),
        );
        q.insert(
            "product".into(),
            HashMap::from([("back".into(), 0.6), ("help".into(), 0.4)]),
        );

        UserArchetype {
            name: "confused_user".into(),
            states,
            transitions: default_transitions(),
            q_values: q,
            temperature: 2.0, // Very random
            population_weight: 0.2,
        }
    }

    fn adversarial_user() -> UserArchetype {
        let states = vec!["home", "search", "product", "cart", "error"]
            .into_iter()
            .map(String::from)
            .collect();
        let mut q = HashMap::new();
        q.insert(
            "home".into(),
            HashMap::from([("inject".into(), 0.8), ("search".into(), 0.2)]),
        );
        q.insert(
            "search".into(),
            HashMap::from([("inject".into(), 0.7), ("boundary".into(), 0.6)]),
        );
        q.insert(
            "product".into(),
            HashMap::from([("tamper".into(), 0.8), ("boundary".into(), 0.5)]),
        );

        UserArchetype {
            name: "adversarial_user".into(),
            states,
            transitions: default_transitions(),
            q_values: q,
            temperature: 0.3, // Focused on attacks
            population_weight: 0.1,
        }
    }
}

fn default_transitions() -> HashMap<String, HashMap<String, Vec<(usize, f64)>>> {
    // Simplified: action maps to next state index with probability 1.0
    HashMap::new() // Transitions handled by select_action + state name matching
}

/// Boltzmann action selection: P(a|s) = exp(Q(s,a)/τ) / Σ exp(Q(s,a')/τ)
fn select_action<R: Rng>(archetype: &UserArchetype, state: &str, rng: &mut R) -> String {
    let actions = match archetype.q_values.get(state) {
        Some(qs) => qs,
        None => return "exit".to_string(),
    };

    if actions.is_empty() {
        return "exit".to_string();
    }

    let tau = archetype.temperature.max(0.01);
    let max_q = actions.values().cloned().fold(f64::NEG_INFINITY, f64::max);

    // Compute Boltzmann probabilities (numerically stable)
    let probs: Vec<(&String, f64)> = actions
        .iter()
        .map(|(a, q)| (a, ((q - max_q) / tau).exp()))
        .collect();

    let total: f64 = probs.iter().map(|(_, p)| p).sum();
    let mut r = rng.gen_range(0.0..1.0) * total;

    for (action, prob) in &probs {
        r -= prob;
        if r <= 0.0 {
            return (*action).clone();
        }
    }

    probs
        .last()
        .map(|(a, _)| (*a).clone())
        .unwrap_or_else(|| "exit".to_string())
}

fn transition<R: Rng>(
    archetype: &UserArchetype,
    _state: &str,
    action: &str,
    _rng: &mut R,
) -> Option<String> {
    // Simple state machine: action name determines next state
    match action {
        "search" => Some("search".into()),
        "browse" => Some("browse".into()),
        "select" => Some("product".into()),
        "add_cart" => Some("cart".into()),
        "checkout" => Some("checkout".into()),
        "pay" => Some("done".into()),
        "back" => Some("home".into()),
        "help" => Some("help".into()),
        "abandon" | "exit" => None,
        "inject" | "tamper" | "boundary" => Some("error".into()),
        "refine" => Some("search".into()),
        "remove" => Some("cart".into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_sessions() {
        let universe = UserUniverse::default_archetypes();
        let sessions = universe.generate_sessions(100, 20);
        assert_eq!(sessions.len(), 100);

        let completed = sessions
            .iter()
            .filter(|s| matches!(s.outcome, SessionOutcome::Completed))
            .count();
        let abandoned = sessions
            .iter()
            .filter(|s| matches!(s.outcome, SessionOutcome::Abandoned))
            .count();
        let errors = sessions
            .iter()
            .filter(|s| matches!(s.outcome, SessionOutcome::Error))
            .count();

        assert!(completed + abandoned + errors == 100);
        assert!(completed > 0, "Should have some completions");
        assert!(abandoned > 0, "Should have some abandonments");
    }

    #[test]
    fn test_boltzmann_low_temperature_is_deterministic() {
        let mut rng = rand::thread_rng();
        let arch = UserUniverse::power_user();

        let mut action_counts: HashMap<String, usize> = HashMap::new();
        for _ in 0..100 {
            let action = select_action(&arch, "checkout", &mut rng);
            *action_counts.entry(action).or_default() += 1;
        }

        // With τ=0.5 and Q(pay)=0.95 >> Q(abandon)=0.05, "pay" should dominate
        let pay_count = action_counts.get("pay").copied().unwrap_or(0);
        assert!(
            pay_count > 80,
            "pay should dominate at low τ: got {}/100",
            pay_count
        );
    }
}
