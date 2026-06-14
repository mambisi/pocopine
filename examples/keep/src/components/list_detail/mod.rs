//! `<keep-list-detail>` — Bear-style list + inline detail layout.
//!
//! Left pane: section title + new-note button + search + filtered
//! note list. Right pane: the shared `<keep-note-form>` driven by
//! `KeepStore::editor_data` / `editor_open`. The component is
//! stateless — it forwards every action to `KeepStore` — and
//! provides the editor-mode context so the embedded form loads
//! from the editor slot rather than the composer slot.

use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::note_form::{KEEP_NOTE_FORM_CONTEXT, KeepNoteForm, KeepNoteFormContext};
use crate::store::KeepStore;

#[derive(Default, Serialize, Deserialize)]
#[component(
    style = "KeepListDetail.css",
    uses = [PineIcon, KeepNoteForm]
)]
pub struct KeepListDetail {}

#[handlers]
impl KeepListDetail {
    pub fn on_setup(&mut self) {
        // The bare `<keep-note-form>` rendered directly in the right
        // pane needs editor-mode load semantics. Providing the
        // context here keeps the wiring local to this shell — the
        // board no longer needs to know about it.
        KEEP_NOTE_FORM_CONTEXT.provide(KeepNoteFormContext::EDITOR_MODAL);
    }

    pub fn open_note(&mut self, note_id: String) {
        pocopine::store::<KeepStore>().update(move |s| s.open_editor(note_id));
        // Mirror Bear / Apple Notes: clicking a row drops the caret
        // straight into the body so the user can type immediately.
        let is_checklist =
            pocopine::store::<KeepStore>().with(|s| s.editor_data.kind == "checklist");
        crate::focus_after_flush(if is_checklist {
            ".list-detail__form .cl-add .cl-txt"
        } else {
            ".list-detail__form textarea.pine-textarea"
        });
    }

    pub fn create_note(&mut self) {
        pocopine::store::<KeepStore>().update(|s| s.create_blank_note("text".into()));
        crate::focus_after_flush(".list-detail__form textarea.pine-textarea");
    }

    pub fn clear_search(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::clear_search);
    }
}
