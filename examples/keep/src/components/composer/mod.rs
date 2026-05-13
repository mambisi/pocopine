//! `<keep-composer>` — the inline "Take a note..." composer.
//!
//! Creation stays inline. The expanded note fields are delegated to
//! `<keep-note-form>`, which is also used inside the modal editor.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::note_form::{KeepNoteFormContext, KEEP_NOTE_FORM_CONTEXT};
use crate::store::KeepStore;

#[derive(Default, Serialize, Deserialize)]
#[component(style = "KeepComposer.css")]
pub struct KeepComposer {}

#[handlers]
impl KeepComposer {
    pub fn on_setup(&mut self) {
        KEEP_NOTE_FORM_CONTEXT.provide(KeepNoteFormContext::DRAFT_COMPOSER);
    }

    pub fn expand_text(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::expand_composer_text);
        crate::focus_after_flush(".composer.expanded textarea");
    }

    pub fn expand_todo(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::expand_composer_todo);
        crate::focus_after_flush(".composer.expanded .cl-add .cl-txt");
    }
}
