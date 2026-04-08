use crate::apply::atomic_write;
use crate::{AppliedEdit, EditError};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Track all edits for undo support.
pub struct EditHistory {
    groups: Vec<EditGroup>,
}

struct EditGroup {
    timestamp: SystemTime,
    turn_number: u32,
    edits: Vec<UndoableEdit>,
}

struct UndoableEdit {
    path: PathBuf,
    before: String,
    after: String,
}

impl EditHistory {
    pub fn new() -> Self {
        Self { groups: Vec::new() }
    }

    /// Start a new edit group for this turn.
    pub fn begin_group(&mut self, turn_number: u32) {
        self.groups.push(EditGroup {
            timestamp: SystemTime::now(),
            turn_number,
            edits: Vec::new(),
        });
    }

    /// Record an applied edit.
    pub fn record(&mut self, edit: &AppliedEdit) {
        if let Some(group) = self.groups.last_mut() {
            group.edits.push(UndoableEdit {
                path: edit.path.clone(),
                before: edit.before_content.clone(),
                after: edit.after_content.clone(),
            });
        }
    }

    /// Undo the last group of edits.
    pub fn undo_last(&mut self, root: &Path) -> Result<Vec<PathBuf>, EditError> {
        let group = self.groups.pop().ok_or(EditError::NothingToUndo)?;

        let mut restored = Vec::new();
        for edit in group.edits.iter().rev() {
            atomic_write(&root.join(&edit.path), &edit.before)?;
            restored.push(edit.path.clone());
        }

        Ok(restored)
    }

    pub fn can_undo(&self) -> bool {
        !self.groups.is_empty()
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
}
