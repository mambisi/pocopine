use pocopine::prelude::*;

use crate::{KeepNote, KeepTag};

use super::{KeepStore, KeepViewMode, labels::normalize_labels, view::KeepFormNote};

impl KeepStore {
    pub(crate) fn save_form_note(&mut self, form: KeepFormNote) {
        let note_id = form.note_id;
        let title = form.title.trim().to_string();
        let body = form.body.trim().to_string();
        // `body_state` is the canonical doc Value — pass it
        // through as-is. `Value::Null` is the "empty / not a
        // text note" sentinel.
        let body_state = form.body_state;
        let color = form.color;
        let todos = form.todos;
        let labels = normalize_labels(form.labels);
        let pinned = form.pinned;
        for label in labels.clone() {
            self.remember_label(&label);
        }

        if note_id.is_empty() {
            if title.is_empty() && body.is_empty() && todos.is_empty() {
                self.composer_open = false;
                return;
            }

            self.notes.clear_error();
            self.next_local_id = self.next_local_id.saturating_add(1);
            let id = format!("note_{}_{}", crate::now_ms(), self.next_local_id);
            let note = KeepNote {
                id: id.clone(),
                title,
                body,
                body_state,
                color,
                pinned: false,
                archived: false,
                todos,
                labels,
                updated_at_ms: crate::now_ms(),
            };

            match self.push_upsert(note, None, "create") {
                Ok(()) => {
                    self.composer_open = false;
                    self.status.clear();
                }
                Err(err) => {
                    self.status = "save failed".into();
                    self.notes.set_error(err.to_string());
                }
            }
            return;
        }

        // In list-detail mode the right pane stays mounted on save,
        // so the form keeps the just-persisted values visible. The
        // modal mode dismisses the dialog as usual.
        if self.view_mode != KeepViewMode::List {
            self.editor_open = false;
            self.clear_editor_fields();
        }
        self.update_note(&note_id, "edit", |note| {
            note.title = title;
            note.body = body;
            note.body_state = body_state;
            note.color = color;
            note.todos = todos;
            note.labels = labels;
            note.pinned = pinned;
        });
    }

    pub(crate) fn archive_form_note(&mut self, form: KeepFormNote) {
        let note_id = form.note_id;
        if note_id.is_empty() {
            return;
        }
        let title = form.title.trim().to_string();
        let body = form.body.trim().to_string();
        let body_state = form.body_state;
        let color = form.color;
        let todos = form.todos;
        let labels = normalize_labels(form.labels);
        for label in labels.clone() {
            self.remember_label(&label);
        }
        self.editor_open = false;
        self.clear_editor_fields();
        self.update_note(&note_id, "archive", |note| {
            note.title = title;
            note.body = body;
            note.body_state = body_state;
            note.color = color;
            note.todos = todos;
            note.labels = labels;
            note.archived = true;
            note.pinned = false;
        });
    }

    pub(super) fn clear_editor_fields(&mut self) {
        self.editor_data = Default::default();
    }

    pub(super) fn update_note(
        &mut self,
        note_id: &str,
        action: &str,
        f: impl FnOnce(&mut KeepNote),
    ) {
        let Some((mut note, base_version)) = self.find_note(note_id) else {
            self.notes.set_error("note is not loaded yet");
            return;
        };
        f(&mut note);
        note.updated_at_ms = crate::now_ms();

        match self.push_upsert(note, base_version, action) {
            Ok(()) => self.status.clear(),
            Err(err) => {
                self.status = "save failed".into();
                self.notes.set_error(err.to_string());
            }
        }
    }

    pub(super) fn find_note(
        &self,
        note_id: &str,
    ) -> Option<(KeepNote, Option<pocopine_sync::RowVersion>)> {
        self.notes
            .rows
            .iter()
            .find(|row| row.value.id == note_id)
            .map(|row| (row.value.clone(), row.version.clone()))
    }

    pub(super) fn update_selection_label(&mut self) {
        let count = self.selected_note_ids.len();
        self.selection_label = match count {
            0 => String::new(),
            1 => "1 selected".to_string(),
            _ => format!("{count} selected"),
        };
    }

    pub(super) fn push_upsert(
        &mut self,
        note: KeepNote,
        base_version: Option<pocopine_sync::RowVersion>,
        action: &str,
    ) -> pocopine_sync::SyncResult<()> {
        let plugins = Plugins;
        let Some(client) = plugins.get::<pocopine_sync::SyncClient>() else {
            return Err(pocopine_sync::SyncError::client(
                "sync plugin not installed",
            ));
        };
        let key = pocopine_sync::RowKey::new(note.id.clone())?;
        let mutation = pocopine_sync::ClientMutation {
            id: pocopine_sync::MutationId::new(format!(
                "keep:{action}:{}:{}:{}",
                note.id,
                crate::now_ms(),
                self.next_local_id
            ))?,
            key: Some(key),
            op: pocopine_sync::SyncOp::Upsert,
            base_version,
            payload: note.clone(),

            migration_outcome: None,
        };
        let optimistic = pocopine_sync::SyncRow::new(note.id.clone(), note)?;

        client
            .collection(pocopine::store::<Self>(), |s: &mut Self| &mut s.notes)
            .stream(crate::KEEP_STREAM)
            .and_then(|c| c.push(mutation, Some(optimistic)))
    }

    pub(super) fn push_delete(
        &mut self,
        note_id: &str,
        base_version: Option<pocopine_sync::RowVersion>,
        action: &str,
    ) -> pocopine_sync::SyncResult<()> {
        let plugins = Plugins;
        let Some(client) = plugins.get::<pocopine_sync::SyncClient>() else {
            return Err(pocopine_sync::SyncError::client(
                "sync plugin not installed",
            ));
        };
        let key = pocopine_sync::RowKey::new(note_id.to_string())?;
        let mutation = pocopine_sync::ClientMutation {
            id: pocopine_sync::MutationId::new(format!(
                "keep:{action}:{note_id}:{}:{}",
                crate::now_ms(),
                self.next_local_id
            ))?,
            key: Some(key),
            op: pocopine_sync::SyncOp::Delete,
            base_version,
            payload: (),

            migration_outcome: None,
        };

        client
            .collection(pocopine::store::<Self>(), |s: &mut Self| &mut s.notes)
            .stream(crate::KEEP_STREAM)
            .and_then(|c| c.push(mutation, None))
    }

    pub(super) fn push_tag_upsert(
        &mut self,
        tag: KeepTag,
        action: &str,
    ) -> pocopine_sync::SyncResult<()> {
        let plugins = Plugins;
        let Some(client) = plugins.get::<pocopine_sync::SyncClient>() else {
            return Err(pocopine_sync::SyncError::client(
                "sync plugin not installed",
            ));
        };
        let key = pocopine_sync::RowKey::new(tag.id.clone())?;
        let mutation = pocopine_sync::ClientMutation {
            id: pocopine_sync::MutationId::new(format!(
                "keep:tag:{action}:{}:{}:{}",
                tag.id,
                crate::now_ms(),
                self.next_local_id
            ))?,
            key: Some(key),
            op: pocopine_sync::SyncOp::Upsert,
            base_version: None,
            payload: tag.clone(),

            migration_outcome: None,
        };
        let optimistic = pocopine_sync::SyncRow::new(tag.id.clone(), tag)?;

        client
            .collection(pocopine::store::<Self>(), |s: &mut Self| &mut s.tags)
            .stream(crate::KEEP_TAGS_STREAM)
            .and_then(|c| c.push(mutation, Some(optimistic)))
    }
}
