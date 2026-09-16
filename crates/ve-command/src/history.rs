use ve_core::Project;

use crate::{Command, CommandError};

/// A command in the history, with the label the UI shows for it.
#[derive(Debug)]
pub struct HistoryEntry {
    pub command: Box<dyn Command>,
}

/// The undo/redo stacks.
///
/// Bounded, because a long session of a professional edit would otherwise hold
/// every clip that was ever deleted. When the limit is reached the *oldest*
/// entry is dropped, which keeps recent work undoable — the opposite choice
/// would make the feature useless.
#[derive(Debug)]
pub struct History {
    undo_stack: Vec<HistoryEntry>,
    redo_stack: Vec<HistoryEntry>,
    limit: usize,
    /// Undo depth as of the last save, used to answer "are there unsaved
    /// changes?" precisely: undoing back to this depth makes the project clean
    /// again, exactly as the user would expect.
    ///
    /// `None` means the saved state has been dropped off the bottom of the
    /// stack, so the project must be assumed dirty.
    saved_depth: Option<usize>,
    /// Whether the entry on top of the undo stack is still accepting merges.
    /// Set by [`History::execute_coalesced`] and cleared by anything that ends
    /// a gesture, so a drag cannot absorb an edit made after the mouse came up.
    merge_open: bool,
}

impl Default for History {
    fn default() -> Self {
        History::new(256)
    }
}

impl History {
    pub fn new(limit: usize) -> Self {
        History {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            limit: limit.max(1),
            saved_depth: Some(0),
            merge_open: false,
        }
    }

    /// Applies a command and, if it succeeds, pushes it onto the undo stack.
    ///
    /// A failed command is dropped: it leaves the project untouched by
    /// contract, so there is nothing to undo.
    pub fn execute(
        &mut self,
        project: &mut Project,
        mut command: Box<dyn Command>,
    ) -> Result<(), CommandError> {
        command.apply(project)?;
        // Any new edit invalidates the redo branch.
        self.redo_stack.clear();
        // A discrete edit always closes an open gesture.
        self.merge_open = false;
        self.push(HistoryEntry { command });
        Ok(())
    }

    /// Like [`History::execute`], but first offers the command to the entry on
    /// top of the stack.
    ///
    /// Used for continuous gestures — dragging, scrubbing a slider — so the
    /// whole gesture collapses into one undo step. Call [`History::break_merge`]
    /// when the gesture ends, so the next one starts a fresh entry.
    pub fn execute_coalesced(
        &mut self,
        project: &mut Project,
        mut command: Box<dyn Command>,
    ) -> Result<(), CommandError> {
        command.apply(project)?;
        self.redo_stack.clear();

        if self.merge_open {
            if let Some(top) = self.undo_stack.last_mut() {
                if top.command.merge(command.as_ref()) {
                    return Ok(());
                }
            }
        }
        self.push(HistoryEntry { command });
        self.merge_open = true;
        Ok(())
    }

    /// Ends a coalescing run, so the next edit cannot merge into the current
    /// top entry. Called on mouse-up.
    pub fn break_merge(&mut self) {
        self.merge_open = false;
    }

    fn push(&mut self, entry: HistoryEntry) {
        self.undo_stack.push(entry);
        if self.undo_stack.len() > self.limit {
            let overflow = self.undo_stack.len() - self.limit;
            self.undo_stack.drain(..overflow);
            // Everything below the saved point has gone, so the saved state is
            // no longer reachable by undoing.
            self.saved_depth = match self.saved_depth {
                Some(d) if d >= overflow => Some(d - overflow),
                _ => None,
            };
        }
    }

    /// Reverts the most recent command, returning its label.
    pub fn undo(&mut self, project: &mut Project) -> Result<String, CommandError> {
        self.merge_open = false;
        let mut entry = self.undo_stack.pop().ok_or(CommandError::NothingToUndo)?;
        match entry.command.undo(project) {
            Ok(()) => {
                let name = entry.command.name().to_string();
                self.redo_stack.push(entry);
                Ok(name)
            }
            Err(e) => {
                // A failed undo means the model and the command disagree. Put
                // the entry back so the stack still describes the project, and
                // let the caller surface the problem.
                self.undo_stack.push(entry);
                Err(e)
            }
        }
    }

    /// Re-applies the most recently undone command, returning its label.
    pub fn redo(&mut self, project: &mut Project) -> Result<String, CommandError> {
        self.merge_open = false;
        let mut entry = self.redo_stack.pop().ok_or(CommandError::NothingToRedo)?;
        match entry.command.apply(project) {
            Ok(()) => {
                let name = entry.command.name().to_string();
                self.undo_stack.push(entry);
                Ok(name)
            }
            Err(e) => {
                self.redo_stack.push(entry);
                Err(e)
            }
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Label of the command undo would revert, for the menu item.
    pub fn undo_name(&self) -> Option<&str> {
        self.undo_stack.last().map(|e| e.command.name())
    }

    pub fn redo_name(&self) -> Option<&str> {
        self.redo_stack.last().map(|e| e.command.name())
    }

    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Records that the project was just saved.
    pub fn mark_saved(&mut self) {
        self.saved_depth = Some(self.undo_stack.len());
    }

    /// Whether the project differs from what was last saved.
    pub fn is_dirty(&self) -> bool {
        self.saved_depth != Some(self.undo_stack.len())
    }

    /// Empties both stacks, for opening a different project.
    pub fn clear(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.saved_depth = Some(0);
        self.merge_open = false;
    }
}
