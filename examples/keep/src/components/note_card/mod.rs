//! `<keep-note-card>` — a single tile in the masonry. Takes scalar
//! note props from the board loop so hydrated rows update directly and
//! every click dispatches into [`KeepStore`] through the local handlers
//! below. Local popover open state stays on the card instance.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    store::{can_create_label, label_picker_options_for, KeepLabelOption},
    KeepStore, KeepTodo,
};

#[derive(Default, Serialize, Deserialize)]
#[component(style = "KeepNoteCard.css")]
pub struct KeepNoteCard {
    #[prop]
    pub note_id: String,
    #[prop]
    pub title: String,
    #[prop]
    pub body: String,
    #[prop]
    pub color: String,
    #[prop]
    pub pinned: bool,
    #[prop]
    pub archived: bool,
    #[prop]
    pub todos: Vec<KeepTodo>,
    #[prop]
    pub labels: Vec<String>,
    #[prop]
    pub pending: bool,
    #[prop]
    pub conflict: bool,

    pub label_picker_open: bool,
    pub label_query: String,
    pub label_options: Vec<KeepLabelOption>,
    pub label_can_create: bool,
    pub color_picker_open: bool,
    #[prop]
    pub selected: bool,
    #[prop]
    pub selection_active: bool,
}

#[handlers]
impl KeepNoteCard {
    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let picker = handle.clone();
        handle.watch_field::<bool, _>("label_picker_open", move |open, _| {
            if *open {
                picker.update(KeepNoteCard::rebuild_label_options);
            }
        });

        let picker = handle.clone();
        handle.watch_field::<String, _>("label_query", move |_, _| {
            picker.update(KeepNoteCard::rebuild_label_options);
        });
    }

    pub fn open(&mut self) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        if pocopine::store::<KeepStore>().with(|s| !s.selected_note_ids.is_empty()) {
            pocopine::store::<KeepStore>().update(move |s| s.toggle_note_selection(id));
            return;
        }
        crate::shared_layout_transition(move |s| s.open_editor(id));
    }

    pub fn toggle_selection(&mut self) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        pocopine::store::<KeepStore>().update(move |s| s.toggle_note_selection(id));
    }

    pub fn pin(&mut self) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        pocopine::store::<KeepStore>().update(move |s| s.toggle_pin(id));
    }

    pub fn archive(&mut self) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        pocopine::store::<KeepStore>().update(move |s| s.toggle_archive(id));
    }

    pub fn check_todo(&mut self, todo_id: String) {
        let note_id = self.note_id.clone();
        if note_id.is_empty() {
            return;
        }
        pocopine::store::<KeepStore>().update(move |s| s.toggle_todo(note_id, todo_id));
    }

    pub fn pick_color(&mut self, color: String) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        self.color_picker_open = false;
        pocopine::store::<KeepStore>().update(move |s| s.set_note_color(id, color));
    }

    pub fn open_labels(&mut self) {
        self.rebuild_label_options();
        self.label_picker_open = true;
    }

    pub fn toggle_label(&mut self, label: String) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        self.toggle_label_option(&label);
        pocopine::store::<KeepStore>().update(move |s| s.toggle_note_label(id, label));
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
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        self.select_label_option(&label);
        pocopine::store::<KeepStore>().update(move |s| s.toggle_note_label(id, label));
    }

    pub fn delete(&mut self) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        pocopine::store::<KeepStore>().update(move |s| s.delete_note(id));
    }

    pub fn copy(&mut self) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        pocopine::store::<KeepStore>().update(move |s| s.copy_note(id));
    }

    pub fn toggle_checkboxes(&mut self) {
        let id = self.note_id.clone();
        if id.is_empty() {
            return;
        }
        pocopine::store::<KeepStore>().update(move |s| s.toggle_note_checklist(id));
    }
}

impl KeepNoteCard {
    fn rebuild_label_options(&mut self) {
        let labels = pocopine::store::<KeepStore>().with(|s| s.labels.clone());
        let (options, can_create) =
            label_picker_options_for(&labels, &self.labels, &self.label_query);
        self.label_options = options;
        self.label_can_create = can_create;
    }

    fn toggle_label_option(&mut self, label: &str) {
        if let Some(option) = self
            .label_options
            .iter_mut()
            .find(|option| option.name == label)
        {
            option.selected = !option.selected;
        }
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
