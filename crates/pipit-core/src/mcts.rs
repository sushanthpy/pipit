//! # MCTS Plan Search (C1)
//!
//! Monte Carlo Tree Search for strategy selection during planning.
//! When the planner generates multiple candidate strategies, MCTS explores
//! them with UCB1 bandit selection and lightweight rollouts to estimate
//! expected value before committing expensive LLM turns.
//!
//! ## Algorithm
//!
//! 1. **Selection**: UCB1 selects the most promising unexplored strategy
//! 2. **Expansion**: Create a child node for the chosen action
//! 3. **Simulation**: Lightweight heuristic rollout (no LLM calls)
//! 4. **Backpropagation**: Update win rates up the tree

use std::collections::HashMap;

/// A node in the MCTS tree.
#[derive(Debug, Clone)]
pub struct MctsNode {
    pub id: u32,
    pub strategy: String,
    pub parent: Option<u32>,
    pub children: Vec<u32>,
    pub visits: u32,
    pub total_reward: f64,
    pub untried_actions: Vec<String>,
}

impl MctsNode {
    pub fn new(id: u32, strategy: &str) -> Self {
        Self {
            id,
            strategy: strategy.to_string(),
            parent: None,
            children: Vec::new(),
            visits: 0,
            total_reward: 0.0,
            untried_actions: Vec::new(),
        }
    }

    /// UCB1 score: exploitation + exploration.
    /// $ \text{UCB1} = \frac{w_i}{n_i} + c \sqrt{\frac{\ln N}{n_i}} $
    pub fn ucb1(&self, parent_visits: u32, exploration_constant: f64) -> f64 {
        if self.visits == 0 {
            return f64::INFINITY;
        }
        let exploitation = self.total_reward / self.visits as f64;
        let exploration = exploration_constant
            * ((parent_visits as f64).ln() / self.visits as f64).sqrt();
        exploitation + exploration
    }

    /// Average reward.
    pub fn avg_reward(&self) -> f64 {
        if self.visits == 0 {
            0.0
        } else {
            self.total_reward / self.visits as f64
        }
    }
}

/// MCTS configuration.
#[derive(Debug, Clone)]
pub struct MctsConfig {
    /// UCB1 exploration constant (√2 is theoretical optimal).
    pub exploration_constant: f64,
    /// Maximum number of MCTS iterations.
    pub max_iterations: u32,
    /// Maximum tree depth.
    pub max_depth: u32,
    /// Reward discount factor per depth level.
    pub discount_factor: f64,
}

impl Default for MctsConfig {
    fn default() -> Self {
        Self {
            exploration_constant: 1.414, // √2
            max_iterations: 100,
            max_depth: 5,
            discount_factor: 0.95,
        }
    }
}

/// Heuristic rollout function type.
/// Given a strategy name and depth, returns an estimated reward in [0, 1].
pub type RolloutFn = Box<dyn Fn(&str, u32) -> f64 + Send>;

/// The MCTS planner.
pub struct MctsPlanner {
    config: MctsConfig,
    nodes: Vec<MctsNode>,
    next_id: u32,
}

impl MctsPlanner {
    pub fn new(config: MctsConfig) -> Self {
        let root = MctsNode::new(0, "root");
        Self {
            config,
            nodes: vec![root],
            next_id: 1,
        }
    }

    /// Add a candidate strategy to the root node.
    pub fn add_candidate(&mut self, strategy: &str) {
        self.nodes[0].untried_actions.push(strategy.to_string());
    }

    /// Run MCTS for the configured number of iterations.
    pub fn search(&mut self, rollout: &RolloutFn) -> MctsResult {
        for _ in 0..self.config.max_iterations {
            // Selection: walk down tree using UCB1
            let selected = self.select(0, 0);

            // Expansion: if there are untried actions, expand
            let expanded = self.expand(selected);

            // Simulation: heuristic rollout
            let node = &self.nodes[expanded as usize];
            let reward = rollout(&node.strategy, 0);

            // Backpropagation: update visits and rewards up to root
            self.backpropagate(expanded, reward);
        }

        self.result()
    }

    /// Run a fixed number of iterations.
    pub fn search_n(&mut self, iterations: u32, rollout: &RolloutFn) -> MctsResult {
        for _ in 0..iterations {
            let selected = self.select(0, 0);
            let expanded = self.expand(selected);
            let node = &self.nodes[expanded as usize];
            let reward = rollout(&node.strategy, 0);
            self.backpropagate(expanded, reward);
        }
        self.result()
    }

    /// Select the best child using UCB1.
    fn select(&self, node_id: u32, depth: u32) -> u32 {
        let node = &self.nodes[node_id as usize];

        // If this node has untried actions or no children, select it
        if !node.untried_actions.is_empty() || node.children.is_empty() {
            return node_id;
        }

        if depth >= self.config.max_depth {
            return node_id;
        }

        // UCB1 selection among children
        let parent_visits = node.visits;
        let best_child = node
            .children
            .iter()
            .max_by(|&&a, &&b| {
                let score_a = self.nodes[a as usize].ucb1(parent_visits, self.config.exploration_constant);
                let score_b = self.nodes[b as usize].ucb1(parent_visits, self.config.exploration_constant);
                score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
            .unwrap_or(node_id);

        self.select(best_child, depth + 1)
    }

    /// Expand a node by adding a child for an untried action.
    fn expand(&mut self, node_id: u32) -> u32 {
        let action = {
            let node = &mut self.nodes[node_id as usize];
            node.untried_actions.pop()
        };

        if let Some(action) = action {
            let child_id = self.next_id;
            self.next_id += 1;

            let mut child = MctsNode::new(child_id, &action);
            child.parent = Some(node_id);
            self.nodes.push(child);
            self.nodes[node_id as usize].children.push(child_id);
            child_id
        } else {
            node_id // No expansion possible
        }
    }

    /// Backpropagate reward from a node up to the root.
    fn backpropagate(&mut self, mut node_id: u32, reward: f64) {
        let mut discounted_reward = reward;
        loop {
            let node = &mut self.nodes[node_id as usize];
            node.visits += 1;
            node.total_reward += discounted_reward;
            discounted_reward *= self.config.discount_factor;

            match node.parent {
                Some(parent) => node_id = parent,
                None => break,
            }
        }
    }

    /// Get the best strategy based on visit count (most robust selection).
    fn result(&self) -> MctsResult {
        let root = &self.nodes[0];
        let best_child = root
            .children
            .iter()
            .max_by_key(|&&id| self.nodes[id as usize].visits)
            .copied();

        let ranked: Vec<StrategyScore> = root
            .children
            .iter()
            .map(|&id| {
                let node = &self.nodes[id as usize];
                StrategyScore {
                    strategy: node.strategy.clone(),
                    visits: node.visits,
                    avg_reward: node.avg_reward(),
                    ucb1: node.ucb1(root.visits, self.config.exploration_constant),
                }
            })
            .collect();

        MctsResult {
            best_strategy: best_child.map(|id| self.nodes[id as usize].strategy.clone()),
            ranked_strategies: ranked,
            total_iterations: root.visits,
            tree_size: self.nodes.len(),
        }
    }

    /// Get tree statistics.
    pub fn stats(&self) -> (usize, u32) {
        (self.nodes.len(), self.nodes[0].visits)
    }
}

/// Result of MCTS search.
#[derive(Debug, Clone)]
pub struct MctsResult {
    pub best_strategy: Option<String>,
    pub ranked_strategies: Vec<StrategyScore>,
    pub total_iterations: u32,
    pub tree_size: usize,
}

/// Score for a candidate strategy.
#[derive(Debug, Clone)]
pub struct StrategyScore {
    pub strategy: String,
    pub visits: u32,
    pub avg_reward: f64,
    pub ucb1: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn constant_rollout(reward: f64) -> RolloutFn {
        Box::new(move |_, _| reward)
    }

    fn strategy_rollout() -> RolloutFn {
        Box::new(|strategy, _| match strategy {
            "minimal_patch" => 0.7,
            "root_cause" => 0.9,
            "architectural" => 0.5,
            _ => 0.3,
        })
    }

    #[test]
    fn basic_search_returns_result() {
        let mut planner = MctsPlanner::new(MctsConfig::default());
        planner.add_candidate("minimal_patch");
        planner.add_candidate("root_cause");

        let result = planner.search_n(50, &constant_rollout(0.8));
        assert!(result.best_strategy.is_some());
        assert_eq!(result.ranked_strategies.len(), 2);
    }

    #[test]
    fn best_strategy_wins() {
        let mut planner = MctsPlanner::new(MctsConfig {
            max_iterations: 200,
            ..Default::default()
        });
        planner.add_candidate("minimal_patch");
        planner.add_candidate("root_cause");
        planner.add_candidate("architectural");

        let result = planner.search(&strategy_rollout());
        assert_eq!(result.best_strategy.as_deref(), Some("root_cause"));
    }

    #[test]
    fn ucb1_infinite_for_unvisited() {
        let node = MctsNode::new(0, "test");
        assert!(node.ucb1(10, 1.414).is_infinite());
    }

    #[test]
    fn ucb1_finite_after_visit() {
        let mut node = MctsNode::new(0, "test");
        node.visits = 5;
        node.total_reward = 3.0;
        assert!(node.ucb1(10, 1.414).is_finite());
        assert!(node.ucb1(10, 1.414) > 0.0);
    }

    #[test]
    fn tree_grows_with_iterations() {
        let mut planner = MctsPlanner::new(MctsConfig::default());
        planner.add_candidate("a");
        planner.add_candidate("b");

        let (size_before, _) = planner.stats();
        planner.search_n(10, &constant_rollout(0.5));
        let (size_after, visits) = planner.stats();

        assert!(size_after >= size_before);
        assert!(visits > 0);
    }

    #[test]
    fn avg_reward_computed_correctly() {
        let mut node = MctsNode::new(0, "test");
        node.visits = 4;
        node.total_reward = 2.0;
        assert!((node.avg_reward() - 0.5).abs() < 1e-10);
    }
}
