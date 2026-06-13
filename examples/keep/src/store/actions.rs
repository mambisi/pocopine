use pocopine::prelude::*;

use crate::KeepTodo;

use super::{
    theme::{load_theme_preference, save_theme_preference},
    view::{format_todo_line, parse_todo_line, KeepEditorData},
    view_mode::save_view_mode_preference,
    KeepStore, KeepViewMode,
};

#[handlers]
impl KeepStore {
    /// Re-assert the semantic defaults that need to be present at first
    /// render. Default::default() already does this at construction;
    /// calling it again from KeepBoard::on_mount is defensive — if the
    /// store proxy's listeners haven't subscribed by the time the
    /// constructor ran (e.g. the very first template walk), this
    /// assignment fires another change notification.
    pub fn ensure_defaults(&mut self) {
        if self.section_kind.is_empty() {
            self.section_kind = "notes".into();
        }
        if self.draft_kind.is_empty() {
            self.draft_kind = "text".into();
        }
        if self.draft_color.is_empty() {
            self.draft_color = "default".into();
        }
        self.theme = load_theme_preference();
        self.labels = self.tag_label_registry();
        self.rebuild_visible_notes();
    }

    // ─── shell ───

    pub fn toggle_sidebar(&mut self) {
        self.sidebar_expanded = !self.sidebar_expanded;
    }

    pub fn set_auth_user(
        &mut self,
        signed_in: bool,
        display_name: String,
        email: String,
        photo_url: String,
    ) {
        self.auth_ready = true;
        self.auth_signed_in = signed_in;
        self.auth_display_name = display_name;
        self.auth_email = email;
        self.auth_photo_url = photo_url;
        self.auth_initial = auth_initial(&self.auth_display_name, &self.auth_email);

        if !signed_in {
            self.clear_selection();
            self.cancel_composer();
            self.cancel_editor();
        }
    }

    pub fn show_notes(&mut self) {
        self.clear_selection();
        self.section_kind = "notes".into();
        self.section_label.clear();
        self.rebuild_visible_notes();
    }

    pub fn show_archive(&mut self) {
        self.clear_selection();
        self.section_kind = "archive".into();
        self.section_label.clear();
        self.rebuild_visible_notes();
    }

    /// Filter the masonry to a single label. Empty `label` falls
    /// back to the default Notes view.
    pub fn show_label(&mut self, label: String) {
        let label = label.trim();
        if label.is_empty() {
            self.clear_selection();
            self.section_kind = "notes".into();
            self.section_label.clear();
            self.rebuild_visible_notes();
            return;
        }
        self.clear_selection();
        self.section_kind = "label".into();
        self.section_label = label.to_string();
        self.rebuild_visible_notes();
    }

    pub fn clear_search(&mut self) {
        self.search_query.clear();
    }

    pub fn toggle_theme(&mut self) {
        self.theme = self.theme.toggled();
        save_theme_preference(self.theme);
    }

    /// Swap the board between masonry and list-detail layouts.
    /// When switching to list mode and no editor is open, select the
    /// first visible note so the right pane isn't empty.
    pub fn cycle_view_mode(&mut self) {
        self.view_mode = self.view_mode.toggled();
        save_view_mode_preference(self.view_mode);
        match self.view_mode {
            KeepViewMode::List => {
                self.clear_selection();
                self.cancel_composer();
                if !self.editor_open
                    && let Some(first) = self
                        .pinned_notes
                        .first()
                        .map(|row| row.value.id.clone())
                        .or_else(|| self.other_notes.first().map(|row| row.value.id.clone()))
                    {
                        self.open_editor(first);
                    }
            }
            KeepViewMode::Masonry => {
                if self.editor_open {
                    self.cancel_editor();
                }
            }
        }
    }

    pub fn add_label(&mut self, label: String) {
        if self.remember_label(&label).is_some() {
            self.sidebar_expanded = true;
        }
    }

    pub fn create_command_label(&mut self) {
        let label = self.command_label_query.trim().to_string();
        if label.is_empty() {
            return;
        }
        self.command_label_query.clear();
        self.add_label(label);
    }

    pub fn refresh(&mut self) {
        // If the local cache had a cursor but no rows (e.g. the
        // server bin lost its in-memory state and the OPFS-cached
        // cursor is now ahead of the server's), an incremental
        // pull comes back empty and the masonry stays empty too.
        // Drop the cursor in that case so the next request asks
        // for a Snapshot and recovers all rows.
        if self.notes.rows.is_empty() && self.notes.cursor.is_some() {
            self.notes.cursor = None;
        }
        if self.tags.rows.is_empty() && self.tags.cursor.is_some() {
            self.tags.cursor = None;
        }
        let plugins = Plugins;
        let Some(client) = plugins.get::<pocopine_sync::SyncClient>() else {
            self.notes.set_error("sync plugin not installed");
            return;
        };
        let notes_result = client
            .collection(pocopine::store::<Self>(), |s: &mut Self| &mut s.notes)
            .stream(crate::KEEP_STREAM)
            .and_then(|c| c.pull());
        let tags_result = client
            .collection(pocopine::store::<Self>(), |s: &mut Self| &mut s.tags)
            .stream(crate::KEEP_TAGS_STREAM)
            .and_then(|c| c.pull());
        if let Err(err) = notes_result {
            self.status = "refresh failed".into();
            self.notes.set_error(err.to_string());
        } else if let Err(err) = tags_result {
            self.status = "refresh failed".into();
            self.notes.set_error(err.to_string());
        }
    }

    /// Force a Snapshot pull regardless of local state — wipes the
    /// in-memory cursor so the next request hits the server with
    /// `cursor: null`. Recovery action when the durable client
    /// cache and the transient server cursor have drifted (rows
    /// gone but a stale cursor still cached).
    pub fn resync(&mut self) {
        self.notes.cursor = None;
        self.tags.cursor = None;
        self.refresh();
    }

    pub fn reset(&mut self) {
        self.resetting = true;
        self.notes.clear_error();
        self.tags.clear_error();
        dispatch!(crate::reset_keep_notes().await, |s, result| {
            s.resetting = false;
            match result {
                Ok(()) => {
                    s.status.clear();
                    s.notes.rows.clear();
                    s.notes.cursor = None;
                    s.tags.rows.clear();
                    s.tags.cursor = None;
                    s.labels.clear();
                    s.clear_selection();
                    s.rebuild_visible_notes();
                }
                Err(err) => {
                    s.status = "reset failed".into();
                    s.notes.set_error(err.to_string());
                }
            }
        });
    }

    // ─── composer ───

    pub fn expand_composer_text(&mut self) {
        self.composer_open = true;
        self.draft_kind = "text".into();
    }

    pub fn expand_composer_todo(&mut self) {
        self.composer_open = true;
        self.draft_kind = "checklist".into();
    }

    pub fn set_draft_color(&mut self, color: String) {
        self.draft_color = color;
    }

    /// Discard the in-progress draft and close the composer without
    /// persisting anything. Bound to Escape from the composer's
    /// inputs.
    pub fn cancel_composer(&mut self) {
        self.composer_open = false;
        self.draft_color = "default".into();
        self.draft_kind = "text".into();
    }

    // ─── card actions ───

    pub fn toggle_pin(&mut self, note_id: String) {
        self.update_note(&note_id, "pin", |note| {
            note.pinned = !note.pinned;
            if note.pinned {
                note.archived = false;
            }
        });
    }

    pub fn toggle_archive(&mut self, note_id: String) {
        self.update_note(&note_id, "archive", |note| {
            note.archived = !note.archived;
            if note.archived {
                note.pinned = false;
            }
        });
    }

    pub fn delete_note(&mut self, note_id: String) {
        let Some((_, base_version)) = self.find_note(&note_id) else {
            self.notes.set_error("note is not loaded yet");
            return;
        };
        match self.push_delete(&note_id, base_version, "delete") {
            Ok(()) => self.status.clear(),
            Err(err) => {
                self.status = "delete failed".into();
                self.notes.set_error(err.to_string());
            }
        }
    }

    pub fn copy_note(&mut self, note_id: String) {
        let Some((mut note, _)) = self.find_note(&note_id) else {
            self.notes.set_error("note is not loaded yet");
            return;
        };
        self.next_local_id = self.next_local_id.saturating_add(1);
        note.id = format!("note_{}_copy_{}", crate::now_ms(), self.next_local_id);
        note.pinned = false;
        note.archived = false;
        note.updated_at_ms = crate::now_ms();

        match self.push_upsert(note, None, "copy") {
            Ok(()) => self.status.clear(),
            Err(err) => {
                self.status = "copy failed".into();
                self.notes.set_error(err.to_string());
            }
        }
    }

    pub fn toggle_note_label(&mut self, note_id: String, label: String) {
        let Some(label) = self.remember_label(&label) else {
            return;
        };
        self.update_note(&note_id, "label", move |note| {
            if let Some(pos) = note.labels.iter().position(|existing| existing == &label) {
                note.labels.remove(pos);
            } else {
                note.labels.push(label);
            }
        });
    }

    pub fn toggle_note_checklist(&mut self, note_id: String) {
        self.update_note(&note_id, "checklist", |note| {
            if note.todos.is_empty() {
                note.todos = note
                    .body
                    .lines()
                    .enumerate()
                    .filter_map(|(index, line)| {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            let (text, done) = parse_todo_line(trimmed);
                            Some(KeepTodo {
                                id: format!("todo_{}_{}", crate::now_ms(), index),
                                text,
                                done,
                            })
                        }
                    })
                    .collect();
                note.body.clear();
            } else {
                let mut lines: Vec<String> = note.todos.iter().map(format_todo_line).collect();
                if !note.body.is_empty() {
                    lines.insert(0, note.body.clone());
                }
                note.body = lines.join("\n");
                note.todos.clear();
            }
        });
    }

    pub fn toggle_todo(&mut self, note_id: String, todo_id: String) {
        self.update_note(&note_id, "todo", |note| {
            if let Some(todo) = note.todos.iter_mut().find(|t| t.id == todo_id) {
                todo.done = !todo.done;
            }
        });
    }

    pub fn set_note_color(&mut self, note_id: String, color: String) {
        self.update_note(&note_id, "color", |note| {
            note.color = color;
        });
    }

    // ─── multi-select ───

    pub fn toggle_note_selection(&mut self, note_id: String) {
        if note_id.is_empty() || self.find_note(&note_id).is_none() {
            return;
        }
        if let Some(pos) = self.selected_note_ids.iter().position(|id| id == &note_id) {
            self.selected_note_ids.remove(pos);
        } else {
            self.selected_note_ids.push(note_id);
        }
        self.update_selection_label();
        self.rebuild_visible_notes();
    }

    pub fn clear_selection(&mut self) {
        if self.selected_note_ids.is_empty() {
            return;
        }
        self.selected_note_ids.clear();
        self.selection_label.clear();
        self.rebuild_visible_notes();
    }

    pub fn pin_selected(&mut self) {
        let ids = self.selected_note_ids.clone();
        for id in ids {
            self.update_note(&id, "pin-selected", |note| {
                note.pinned = true;
                note.archived = false;
            });
        }
        self.clear_selection();
    }

    pub fn archive_selected(&mut self) {
        let ids = self.selected_note_ids.clone();
        for id in ids {
            self.update_note(&id, "archive-selected", |note| {
                note.archived = true;
                note.pinned = false;
            });
        }
        self.clear_selection();
    }

    pub fn delete_selected(&mut self) {
        let ids = self.selected_note_ids.clone();
        for id in ids {
            self.delete_note(id);
        }
        self.clear_selection();
    }

    pub fn set_selected_color(&mut self, color: String) {
        let ids = self.selected_note_ids.clone();
        for id in ids {
            let color = color.clone();
            self.update_note(&id, "color-selected", move |note| {
                note.color = color;
            });
        }
    }

    // ─── list-detail ───

    /// Create an empty note and open it in the list-detail right
    /// pane. Bear-style flow: clicking + spawns a blank row the
    /// user can immediately type into. `kind` decides whether the
    /// inline form opens in text or checklist mode.
    ///
    /// Snaps the section back to Notes and clears any active
    /// search query so the freshly created row is guaranteed to
    /// be visible in the left pane. Otherwise a `+` press from
    /// the Archive section, or while the user has typed a
    /// search term, would create a note that doesn't match the
    /// current view filter and never shows up as selected.
    pub fn create_blank_note(&mut self, kind: String) {
        self.search_query.clear();
        self.section_kind = "notes".into();
        self.section_label.clear();
        self.clear_selection();
        self.cancel_composer();
        self.next_local_id = self.next_local_id.saturating_add(1);
        let id = format!("note_{}_{}", crate::now_ms(), self.next_local_id);
        let note = crate::KeepNote {
            id: id.clone(),
            title: String::new(),
            body: String::new(),
            body_state: None,
            color: "default".into(),
            pinned: false,
            archived: false,
            todos: Vec::new(),
            labels: Vec::new(),
            updated_at_ms: crate::now_ms(),
        };
        // `push_upsert` lands its optimistic row asynchronously
        // (it runs inside `spawn_for_scope`), so calling
        // `open_editor(id)` right after would hit `notes.rows`
        // before the row arrives and silently no-op — the new
        // note would never become the selected detail. Set
        // `editor_data` directly from the local `note` value
        // instead, then push. The form's `editor_data` watcher
        // fires on the id change and loads immediately; the
        // optimistic row joins `pinned_notes`/`other_notes`
        // moments later via the usual rebuild and the list-row
        // `:data-on=` finds its match.
        self.editor_data = KeepEditorData::from_note(note.clone());
        if kind == "checklist" {
            // KeepEditorData::from_note derives kind from
            // todos.is_empty(); a fresh note has no todos so it
            // would always land in "text".
            self.editor_data.kind = "checklist".into();
        }
        self.editor_open = true;

        match self.push_upsert(note, None, "create") {
            Ok(()) => {
                self.status.clear();
                self.notes.clear_error();
            }
            Err(err) => {
                self.status = "save failed".into();
                self.notes.set_error(err.to_string());
                self.editor_open = false;
                self.clear_editor_fields();
            }
        }
    }

    pub fn set_search_query(&mut self, query: String) {
        self.search_query = query;
        self.rebuild_visible_notes();
    }

    // ─── editor (detail dialog) ───

    pub fn open_editor(&mut self, note_id: String) {
        self.clear_selection();
        self.cancel_composer();
        let Some(row) = self.notes.rows.iter().find(|r| r.value.id == note_id) else {
            return;
        };
        self.editor_data = KeepEditorData::from_note(row.value.clone());
        self.editor_open = true;
    }

    pub fn cancel_editor(&mut self) {
        self.editor_open = false;
        self.clear_editor_fields();
    }

    pub fn set_editor_color(&mut self, color: String) {
        self.editor_data.color = color;
    }

    pub fn toggle_editor_pin(&mut self) {
        self.editor_data.pinned = !self.editor_data.pinned;
    }
}

pub(crate) fn auth_initial(display_name: &str, email: &str) -> String {
    display_name
        .chars()
        .chain(email.chars())
        .find(|ch| !ch.is_whitespace())
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_else(|| "G".to_string())
}
