//! Shared note editor used by the inline composer and modal editor.
//!
//! The component owns one local edit buffer. Shells provide context
//! (`draft` vs `editor`) and the form dispatches either a create or update
//! when the buffer is saved.

use pocopine::create_context;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::store::{
    can_create_label, format_todo_line, label_picker_options_for, parse_todo_line, KeepEditorData,
    KeepFormNote, KeepLabelOption, KeepStore, KeepViewMode,
};
use crate::KeepTodo;

create_context!(pub(crate) KEEP_NOTE_FORM_CONTEXT: KeepNoteFormContext);

#[derive(Clone, Copy)]
pub(crate) struct KeepNoteFormContext {
    pub mode: &'static str,
}

impl KeepNoteFormContext {
    pub const DRAFT_COMPOSER: Self = Self { mode: "draft" };

    pub const EDITOR_MODAL: Self = Self { mode: "editor" };
}

#[derive(Default, Serialize, Deserialize)]
#[component(style = "KeepNoteForm.css")]
pub struct KeepNoteForm {
    pub mode: String,
    pub note_id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
    pub color: String,
    pub todos: Vec<KeepTodo>,
    pub labels: Vec<String>,
    pub pinned: bool,
    pub todo_text: String,
    pub label_picker_open: bool,
    pub label_query: String,
    pub label_options: Vec<KeepLabelOption>,
    pub label_can_create: bool,
    pub color_picker_open: bool,
    pub next_todo_id: u64,
}

#[handlers]
impl KeepNoteForm {
    pub fn on_setup(&mut self) {
        let ctx = KEEP_NOTE_FORM_CONTEXT
            .inject()
            .unwrap_or(KeepNoteFormContext::DRAFT_COMPOSER);
        self.mode = ctx.mode.into();
        self.reset_draft("text");
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        if self.mode == "draft" {
            let h = handle.clone();
            pocopine::store::<KeepStore>().watch_field::<bool, _>(
                "composer_open",
                move |open, prev| {
                    let was_open = prev.copied().unwrap_or(false);
                    if *open && !was_open {
                        schedule_prepare_draft(h.clone());
                    } else if !*open && was_open {
                        schedule_reset_draft(h.clone());
                    }
                },
            );

            let h = handle.clone();
            pocopine::store::<KeepStore>().watch_field::<String, _>("draft_kind", move |_, _| {
                schedule_prepare_draft(h.clone());
            });
        } else {
            let h = handle.clone();
            pocopine::store::<KeepStore>().watch_field::<bool, _>(
                "editor_open",
                move |open, prev| {
                    let was_open = prev.copied().unwrap_or(false);
                    if *open && !was_open {
                        schedule_load_editor(h.clone());
                    }
                },
            );

            let h = handle.clone();
            pocopine::store::<KeepStore>().watch_field::<KeepEditorData, _>(
                "editor_data",
                move |data, prev| {
                    let previous = prev.cloned().unwrap_or_default();
                    if data.id != previous.id {
                        // List-detail row switch: persist the form's
                        // local edits against the previous note before
                        // overwriting them with the new row's data.
                        let is_list = pocopine::store::<KeepStore>()
                            .with(|s| s.view_mode == KeepViewMode::List);
                        if is_list && !previous.id.is_empty() {
                            schedule_save_then_load(h.clone());
                        } else {
                            schedule_load_editor(h.clone());
                        }
                    } else if data.pinned != previous.pinned {
                        schedule_sync_editor_pin(h.clone());
                    }
                },
            );

            // Late-mount case: when entering list-detail mode the
            // store auto-opens the first visible note *before* this
            // form mounts, so the watchers above are registered
            // after the false→true editor_open transition has
            // already happened. Trigger an explicit load so the
            // right pane shows the active note on first render.
            // The helper is a no-op when editor_open is false.
            schedule_load_editor(handle.clone());
        }

        let picker = handle.clone();
        handle.watch_field::<bool, _>("label_picker_open", move |open, _| {
            if *open {
                picker.update(KeepNoteForm::rebuild_label_options);
            }
        });

        let picker = handle.clone();
        handle.watch_field::<String, _>("label_query", move |_, _| {
            picker.update(KeepNoteForm::rebuild_label_options);
        });
    }

    pub fn save(&mut self) {
        if !self.is_active() {
            return;
        }
        self.close_popovers();
        let form = self.collect_fields();
        if self.mode == "editor" {
            crate::shared_layout_transition(move |s| {
                s.save_form_note(form);
            });
        } else {
            pocopine::store::<KeepStore>().update(move |s| {
                s.save_form_note(form);
            });
        }
    }

    /// Click-outside auto-save. In the list-detail pane the right
    /// pane is permanently mounted, so every click on the topbar,
    /// sidebar, or list rows would otherwise generate a no-op
    /// upsert. List mode auto-saves on row switch instead (handled
    /// by the editor_data id watcher in `on_ready`).
    pub fn auto_save(&mut self) {
        if pocopine::store::<KeepStore>().with(|s| s.view_mode == KeepViewMode::List) {
            return;
        }
        self.save();
    }

    pub fn cancel(&mut self) {
        self.close_popovers();
        if self.mode == "editor" {
            crate::shared_layout_transition(KeepStore::cancel_editor);
        } else {
            self.reset_draft("text");
            pocopine::store::<KeepStore>().update(KeepStore::cancel_composer);
        }
    }

    pub fn toggle_kind(&mut self) {
        if self.kind == "checklist" {
            let mut lines: Vec<String> = self.todos.iter().map(format_todo_line).collect();
            if !self.body.is_empty() {
                lines.insert(0, self.body.clone());
            }
            self.body = lines.join("\n");
            self.todos.clear();
            self.kind = "text".into();
        } else {
            let lines: Vec<String> = self.body.lines().map(str::to_string).collect();
            for line in lines {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let (text, done) = parse_todo_line(trimmed);
                self.push_todo(text, done);
            }
            self.body.clear();
            self.kind = "checklist".into();
        }
    }

    pub fn pick_color(&mut self, color: String) {
        self.color_picker_open = false;
        self.color = color.clone();
        if self.mode == "editor" {
            pocopine::store::<KeepStore>().update(move |s| s.set_editor_color(color));
        } else {
            pocopine::store::<KeepStore>().update(move |s| s.set_draft_color(color));
        }
    }

    pub fn archive(&mut self) {
        if self.mode != "editor" || !self.is_active() {
            return;
        }
        self.close_popovers();
        let form = self.collect_fields();
        crate::shared_layout_transition(move |s| {
            s.archive_form_note(form);
        });
    }

    pub fn toggle_label(&mut self, label: String) {
        if let Some(pos) = self.labels.iter().position(|existing| existing == &label) {
            self.labels.remove(pos);
        } else {
            self.labels.push(label.clone());
            pocopine::store::<KeepStore>().update(move |s| s.add_label(label));
        }
        self.rebuild_label_options();
    }

    pub fn create_label(&mut self) {
        let label = self.label_query.trim().to_string();
        if label.is_empty() {
            return;
        }
        let labels = pocopine::store::<KeepStore>().with(|s| s.labels.clone());
        if !can_create_label(&labels, &label) {
            return;
        }
        self.label_query.clear();
        if !self.labels.iter().any(|existing| existing == &label) {
            self.labels.push(label.clone());
        }
        self.select_label_option(&label);
        pocopine::store::<KeepStore>().update(move |s| s.add_label(label));
    }
}

fn schedule_prepare_draft(handle: pocopine::Handle<KeepNoteForm>) {
    pocopine::tick::next(move || {
        handle.update(KeepNoteForm::prepare_draft_from_store);
    });
}

fn schedule_reset_draft(handle: pocopine::Handle<KeepNoteForm>) {
    pocopine::tick::next(move || {
        handle.update(|form| form.reset_draft("text"));
    });
}

fn schedule_load_editor(handle: pocopine::Handle<KeepNoteForm>) {
    pocopine::tick::next(move || {
        handle.update(KeepNoteForm::load_editor_from_store);
    });
}

fn schedule_save_then_load(handle: pocopine::Handle<KeepNoteForm>) {
    pocopine::tick::next(move || {
        handle.update(|form| {
            let snapshot = form.collect_fields();
            if !snapshot.note_id.is_empty() {
                pocopine::store::<KeepStore>().update(move |s| s.save_form_note(snapshot));
            }
            form.load_editor_from_store();
        });
    });
}

fn schedule_sync_editor_pin(handle: pocopine::Handle<KeepNoteForm>) {
    pocopine::tick::next(move || {
        handle.update(KeepNoteForm::sync_editor_pin);
    });
}

impl KeepNoteForm {
    fn close_popovers(&mut self) {
        self.color_picker_open = false;
        self.label_picker_open = false;
    }

    fn is_active(&self) -> bool {
        let mode = self.mode.clone();
        pocopine::store::<KeepStore>().with(|s| {
            if mode == "editor" {
                s.editor_open
            } else {
                s.composer_open
            }
        })
    }

    fn prepare_draft_from_store(&mut self) {
        if self.mode != "draft" {
            return;
        }
        let Some(kind) = pocopine::store::<KeepStore>().with(|s| {
            s.composer_open.then(|| {
                if s.draft_kind == "checklist" {
                    "checklist".to_string()
                } else {
                    "text".to_string()
                }
            })
        }) else {
            return;
        };
        self.reset_draft(&kind);
    }

    fn reset_draft(&mut self, kind: &str) {
        self.note_id.clear();
        self.title.clear();
        self.body.clear();
        self.kind = if kind == "checklist" {
            "checklist".into()
        } else {
            "text".into()
        };
        self.color = "default".into();
        self.todos.clear();
        self.labels.clear();
        self.pinned = false;
        self.todo_text.clear();
        self.label_query.clear();
        self.label_options.clear();
        self.label_can_create = false;
        self.close_popovers();
    }

    fn load_editor_from_store(&mut self) {
        if self.mode != "editor" {
            return;
        }
        let Some(data) =
            pocopine::store::<KeepStore>().with(|s| s.editor_open.then(|| s.editor_data.clone()))
        else {
            return;
        };
        self.note_id = data.id;
        self.kind = if data.kind == "checklist" {
            "checklist".into()
        } else {
            "text".into()
        };
        self.title = data.title;
        self.body = data.body;
        self.color = if data.color.is_empty() {
            "default".into()
        } else {
            data.color
        };
        self.todos = data.todos;
        self.labels = data.labels;
        self.pinned = data.pinned;
        self.todo_text.clear();
        self.label_query.clear();
        self.rebuild_label_options();
        self.close_popovers();
    }

    fn sync_editor_pin(&mut self) {
        if self.mode != "editor" {
            return;
        }
        if let Some(pinned) =
            pocopine::store::<KeepStore>().with(|s| s.editor_open.then_some(s.editor_data.pinned))
        {
            self.pinned = pinned;
        }
    }

    fn collect_fields(&mut self) -> KeepFormNote {
        let mut todos = self.todos.clone();
        let inline = self.todo_text.trim().to_string();
        if self.kind == "checklist" && !inline.is_empty() {
            self.next_todo_id = self.next_todo_id.saturating_add(1);
            todos.push(KeepTodo {
                id: format!("todo_{}_{}", crate::now_ms(), self.next_todo_id),
                text: inline,
                done: false,
            });
            self.todo_text.clear();
        }
        KeepFormNote {
            note_id: self.note_id.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            color: self.color.clone(),
            todos,
            labels: self.labels.clone(),
            pinned: self.pinned,
        }
    }

    fn push_todo(&mut self, text: String, done: bool) {
        self.next_todo_id = self.next_todo_id.saturating_add(1);
        self.todos.push(KeepTodo {
            id: format!("todo_{}_{}", crate::now_ms(), self.next_todo_id),
            text,
            done,
        });
    }

    fn rebuild_label_options(&mut self) {
        let labels = pocopine::store::<KeepStore>().with(|s| s.labels.clone());
        let (options, can_create) =
            label_picker_options_for(&labels, &self.labels, &self.label_query);
        self.label_options = options;
        self.label_can_create = can_create;
    }

    fn select_label_option(&mut self, label: &str) {
        if let Some(option) = self
            .label_options
            .iter_mut()
            .find(|option| option.name == label)
        {
            option.selected = true;
        } else {
            self.label_options.push(KeepLabelOption {
                name: label.to_string(),
                selected: true,
                visible: true,
            });
        }
    }
}
