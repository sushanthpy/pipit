use crate::ContextError;
use pipit_provider::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::SystemTime;

/// Unique branch identifier.
pub type BranchId = String;

/// A session is a tree of message sequences (branching conversations).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTree {
    pub root: BranchId,
    pub branches: HashMap<BranchId, Branch>,
    pub active_branch: BranchId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Branch {
    pub id: BranchId,
    pub parent: Option<BranchId>,
    pub fork_point: usize,
    pub messages: Vec<Message>,
    pub label: Option<String>,
    pub created_at: SystemTime,
}

impl SessionTree {
    pub fn new() -> Self {
        let root_id = uuid::Uuid::new_v4().to_string();
        let mut branches = HashMap::new();
        branches.insert(
            root_id.clone(),
            Branch {
                id: root_id.clone(),
                parent: None,
                fork_point: 0,
                messages: Vec::new(),
                label: Some("main".to_string()),
                created_at: SystemTime::now(),
            },
        );

        Self {
            root: root_id.clone(),
            branches,
            active_branch: root_id,
        }
    }

    /// Fork the current branch at the current position.
    pub fn fork(&mut self, label: Option<String>) -> BranchId {
        let current = &self.branches[&self.active_branch];
        let fork_point = current.messages.len();
        let new_id = uuid::Uuid::new_v4().to_string();

        self.branches.insert(
            new_id.clone(),
            Branch {
                id: new_id.clone(),
                parent: Some(self.active_branch.clone()),
                fork_point,
                messages: Vec::new(),
                label,
                created_at: SystemTime::now(),
            },
        );

        self.active_branch = new_id.clone();
        new_id
    }

    /// Push a message to the active branch.
    pub fn push_message(&mut self, message: Message) {
        if let Some(branch) = self.branches.get_mut(&self.active_branch) {
            branch.messages.push(message);
        }
    }

    /// Get the full message history for the active branch,
    /// walking up the parent chain.
    pub fn active_messages(&self) -> Vec<&Message> {
        let mut chain = Vec::new();
        let mut branch_id = self.active_branch.clone();

        loop {
            let branch = match self.branches.get(&branch_id) {
                Some(b) => b,
                None => break,
            };
            chain.push(branch);
            match &branch.parent {
                Some(parent_id) => branch_id = parent_id.clone(),
                None => break,
            }
        }

        chain.reverse();
        let mut messages = Vec::new();

        for (i, branch) in chain.iter().enumerate() {
            if i == 0 {
                messages.extend(branch.messages.iter());
            } else {
                let fork = branch.fork_point;
                messages.truncate(fork);
                messages.extend(branch.messages.iter());
            }
        }

        messages
    }

    /// Switch to a different branch.
    pub fn switch(&mut self, target: BranchId) -> Result<(), ContextError> {
        if !self.branches.contains_key(&target) {
            return Err(ContextError::BranchNotFound(target));
        }
        self.active_branch = target;
        Ok(())
    }

    /// List all branches.
    pub fn list_branches(&self) -> Vec<(&str, &str, usize)> {
        self.branches
            .values()
            .map(|b| {
                (
                    b.id.as_str(),
                    b.label.as_deref().unwrap_or("(unnamed)"),
                    b.messages.len(),
                )
            })
            .collect()
    }
}
