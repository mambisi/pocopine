use crate::KeepNote;

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

            self.status.clear();
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

            // Fire-and-forget create: the write helper spawns the
            // push and surfaces any failure via `self.status` from
            // the spawned task. Optimistic success closes the
            // composer immediately.
            crate::sync::write_note(KeepNote::create(note.id.clone(), note.to_draft()), "create");
            self.composer_open = false;
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
            self.status = "note is not loaded yet".to_string();
            return;
        };
        f(&mut note);
        note.updated_at_ms = crate::now_ms();
        self.status.clear();

        // A note's optimistic-concurrency version is its
        // `updated_at_ms`. With a base version this is an `update`
        // (conditional on the last-seen version); without one it's
        // a fresh `create`.
        if base_version.is_some() {
            crate::sync::write_note(
                KeepNote::update(note.id.clone(), note.to_draft(), base_version),
                action,
            );
        } else {
            crate::sync::write_note(KeepNote::create(note.id.clone(), note.to_draft()), action);
        }
    }

    pub(super) fn find_note(
        &self,
        note_id: &str,
    ) -> Option<(KeepNote, Option<pocopine_sync::RowVersion>)> {
        self.notes.iter().find(|n| n.id == note_id).map(|n| {
            let v = pocopine_sync::RowVersion::new(n.updated_at_ms.to_string()).ok();
            (n.clone(), v)
        })
    }

    pub(super) fn update_selection_label(&mut self) {
        let count = self.selected_note_ids.len();
        self.selection_label = match count {
            0 => String::new(),
            1 => "1 selected".to_string(),
            _ => format!("{count} selected"),
        };
    }
}
