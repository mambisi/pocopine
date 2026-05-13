//! `<keep-editor>` — the detail dialog shell. The shared inner fields
//! and toolbar live in `<keep-note-form>`.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::note_form::{KeepNoteFormContext, KEEP_NOTE_FORM_CONTEXT};
use crate::store::KeepStore;

#[derive(Default, Serialize, Deserialize)]
#[component(style = "KeepEditor.css")]
pub struct KeepEditor {}

#[handlers]
impl KeepEditor {
    pub fn on_setup(&mut self) {
        KEEP_NOTE_FORM_CONTEXT.provide(KeepNoteFormContext::EDITOR_MODAL);
    }

    pub fn on_ready(&self, _handle: pocopine::Handle<Self>) {
        pocopine::store::<KeepStore>().watch_field::<bool, _>("editor_open", move |open, prev| {
            if !*open || prev.copied().unwrap_or(false) {
                return;
            }
            let selector = pocopine::store::<KeepStore>().with(|s| {
                if s.editor_data.kind == "checklist" {
                    ".note-body-checklist .cl-add .cl-txt"
                } else {
                    ".modal-note textarea"
                }
            });
            crate::focus_after_flush(selector);
        });
    }

    pub fn cancel(&mut self) {
        crate::shared_layout_transition(KeepStore::cancel_editor);
    }

    pub fn toggle_pin(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::toggle_editor_pin);
    }
}
