use serde::{Deserialize, Serialize};

use crate::history::{History, HistoryEntry};
use crate::{
    ChangeSet, CodeError, CodeResult, DocumentLimits, DocumentRevision, HistoryLimits, Selection,
    TextDocument,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditOrigin {
    Input,
    Composition,
    Paste,
    Drop,
    Cut,
    Command,
    Undo,
    Redo,
    Api,
    Load,
}

#[derive(Clone, Debug)]
pub struct Transaction {
    pub base_revision: DocumentRevision,
    pub changes: ChangeSet,
    pub selection: Option<Selection>,
    pub origin: EditOrigin,
}

#[derive(Clone, Debug)]
pub struct EditorState {
    document: TextDocument,
    selection: Selection,
    revision: DocumentRevision,
}

impl EditorState {
    pub fn document(&self) -> &TextDocument {
        &self.document
    }
    pub fn selection(&self) -> Selection {
        self.selection
    }
    pub fn revision(&self) -> DocumentRevision {
        self.revision
    }
    pub fn transaction(&self, changes: ChangeSet, origin: EditOrigin) -> Transaction {
        Transaction {
            base_revision: self.revision,
            changes,
            selection: None,
            origin,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CodeChange {
    pub view_status: crate::ViewStatus,
    pub origin: EditOrigin,
    pub before_revision: DocumentRevision,
    pub revision: DocumentRevision,
    /// Ranges in the pre-edit document. Inserted content stays in the model.
    pub ranges: Vec<crate::TextRange>,
    pub inserted_lengths: Vec<usize>,
    pub selection: Selection,
    pub document_changed: bool,
    pub selection_changed: bool,
    pub can_undo: bool,
    pub can_redo: bool,
    pub history_retained: bool,
    pub normalized: bool,
}

/// The single owner of committed text, revision, selection, and undo history.
pub struct Editor {
    state: EditorState,
    limits: DocumentLimits,
    history: History,
}

impl Editor {
    pub fn new(text: &str, limits: DocumentLimits, history: HistoryLimits) -> CodeResult<Self> {
        Ok(Self {
            state: EditorState {
                document: TextDocument::new(text, limits)?,
                selection: Selection::default(),
                revision: DocumentRevision(0),
            },
            limits,
            history: History::new(history)?,
        })
    }

    pub fn state(&self) -> &EditorState {
        &self.state
    }
    pub fn limits(&self) -> DocumentLimits {
        self.limits
    }
    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }
    pub fn history_retained_bytes(&self) -> usize {
        self.history.retained_bytes()
    }
    pub fn close_history_group(&mut self) {
        self.history.close_group();
    }

    pub fn check_revision(&self, expected: DocumentRevision) -> CodeResult<()> {
        if expected != self.state.revision {
            Err(CodeError::StaleRevision {
                expected,
                actual: self.state.revision,
            })
        } else {
            Ok(())
        }
    }

    fn next_state(&self, transaction: &Transaction) -> CodeResult<EditorState> {
        self.check_revision(transaction.base_revision)?;
        let document = transaction
            .changes
            .apply(&self.state.document, self.limits)?;
        let selection = match transaction.selection {
            Some(selection) => selection,
            None => transaction
                .changes
                .map_selection(&self.state.document, self.state.selection)?,
        };
        document.check_selection(selection)?;
        let mut revision = self.state.revision;
        if document.text() != self.state.document.text() {
            revision.0 = revision
                .0
                .checked_add(1)
                .ok_or(CodeError::RevisionExhausted)?;
        }
        Ok(EditorState {
            document,
            selection,
            revision,
        })
    }

    fn update(
        &self,
        old: &EditorState,
        changes: &ChangeSet,
        origin: EditOrigin,
        retained: bool,
        normalized: bool,
    ) -> CodeChange {
        CodeChange {
            view_status: crate::ViewStatus::Ready,
            origin,
            before_revision: old.revision,
            revision: self.state.revision,
            ranges: changes.changes().iter().map(|c| c.range).collect(),
            inserted_lengths: changes.changes().iter().map(|c| c.insert.len()).collect(),
            selection: self.state.selection,
            document_changed: old.document != self.state.document,
            selection_changed: old.selection != self.state.selection,
            can_undo: self.can_undo(),
            can_redo: self.can_redo(),
            history_retained: retained,
            normalized,
        }
    }

    pub fn dispatch(&mut self, transaction: Transaction, time_ms: u64) -> CodeResult<CodeChange> {
        // History/load origins cannot be used to bypass the history owner.
        if matches!(
            transaction.origin,
            EditOrigin::Undo | EditOrigin::Redo | EditOrigin::Load
        ) {
            return Err(CodeError::InvalidConfiguration);
        }
        let next = self.next_state(&transaction)?;
        let changed = next.document != self.state.document;
        let retained = if changed {
            let inverse = transaction
                .changes
                .inverse(&self.state.document, self.limits)?;
            self.history.record(
                HistoryEntry {
                    forward: transaction.changes.clone(),
                    backward: inverse,
                    before: self.state.selection,
                    after: next.selection,
                },
                transaction.origin,
                time_ms,
            )
        } else {
            if next.selection != self.state.selection {
                self.history.close_group();
            }
            false
        };
        let old = std::mem::replace(&mut self.state, next);
        Ok(self.update(
            &old,
            &transaction.changes,
            transaction.origin,
            retained,
            false,
        ))
    }

    pub fn set_selection(
        &mut self,
        selection: Selection,
        revision: DocumentRevision,
    ) -> CodeResult<CodeChange> {
        self.dispatch(
            Transaction {
                base_revision: revision,
                changes: ChangeSet::default(),
                selection: Some(selection),
                origin: EditOrigin::Api,
            },
            0,
        )
    }

    pub fn load_document(
        &mut self,
        text: &str,
        revision: DocumentRevision,
    ) -> CodeResult<CodeChange> {
        self.check_revision(revision)?;
        let document = TextDocument::new(text, self.limits)?;
        let changes = ChangeSet::between(&self.state.document, document.text())?;
        let next_revision = DocumentRevision(
            revision
                .0
                .checked_add(1)
                .ok_or(CodeError::RevisionExhausted)?,
        );
        let normalized = document.text() != text;
        let old = std::mem::replace(
            &mut self.state,
            EditorState {
                document,
                selection: Selection::default(),
                revision: next_revision,
            },
        );
        self.history.clear();
        Ok(self.update(&old, &changes, EditOrigin::Load, false, normalized))
    }

    pub fn undo(&mut self) -> CodeResult<Option<CodeChange>> {
        self.travel_history(false)
    }
    pub fn redo(&mut self) -> CodeResult<Option<CodeChange>> {
        self.travel_history(true)
    }

    fn travel_history(&mut self, redo: bool) -> CodeResult<Option<CodeChange>> {
        let Some(group) = self.history.peek(redo) else {
            return Ok(None);
        };
        let mut document = self.state.document.clone();
        if redo {
            for entry in &group.entries {
                document = entry.forward.apply(&document, self.limits)?;
            }
        } else {
            for entry in group.entries.iter().rev() {
                document = entry.backward.apply(&document, self.limits)?;
            }
        }
        let selection = if redo {
            group.entries.last().unwrap().after
        } else {
            group.entries[0].before
        };
        let changes = ChangeSet::between(&self.state.document, document.text())?;
        let origin = if redo {
            EditOrigin::Redo
        } else {
            EditOrigin::Undo
        };
        let transaction = Transaction {
            base_revision: self.state.revision,
            changes,
            selection: Some(selection),
            origin,
        };
        let next = self.next_state(&transaction)?;
        self.history.travel(redo);
        let old = std::mem::replace(&mut self.state, next);
        Ok(Some(self.update(
            &old,
            &transaction.changes,
            origin,
            true,
            false,
        )))
    }
}
