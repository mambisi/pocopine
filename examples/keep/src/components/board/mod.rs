//! `<keep-board>` — the top-level Keep app shell and sync wiring.

use pine::{
    PineCommandContent, PineCommandEmpty, PineCommandInput, PineCommandItem, PineCommandList,
    PineCommandOverlay, PineCommandPortal, PineCommandRoot, PineDropdownMenuContent,
    PineDropdownMenuItem, PineDropdownMenuLabel, PineDropdownMenuPortal, PineDropdownMenuRoot,
    PineDropdownMenuSeparator, PineDropdownMenuTrigger, PineInput, PinePopoverContent,
    PinePopoverPortal, PinePopoverRoot, PinePopoverTrigger, PineTextarea,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    composer::KeepComposer, editor::KeepEditor, note_body::KeepNoteBody, note_card::KeepNoteCard,
    note_form::KeepNoteForm,
};
use crate::{focus_after_flush, KeepStore, KEEP_STREAM, KEEP_TAGS_STREAM};

/// Top-level component. Owns nothing except the sync wiring; every
/// piece of durable UI state lives in [`KeepStore`].
#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "KeepBoard.poco",
    style = "KeepBoard.css",
    role = "panel",
    uses = [
        PineIcon,
        KeepComposer,
        KeepNoteCard,
        KeepEditor,
        KeepNoteForm,
        KeepNoteBody,
        PineCommandRoot,
        PineCommandPortal,
        PineCommandOverlay,
        PineCommandContent,
        PineCommandInput,
        PineCommandList,
        PineCommandItem,
        PineCommandEmpty,
        PineDropdownMenuRoot,
        PineDropdownMenuTrigger,
        PineDropdownMenuPortal,
        PineDropdownMenuContent,
        PineDropdownMenuItem,
        PineDropdownMenuSeparator,
        PineDropdownMenuLabel,
        PineInput,
        PinePopoverRoot,
        PinePopoverTrigger,
        PinePopoverPortal,
        PinePopoverContent,
        PineTextarea,
    ]
)]
pub struct KeepBoard {
    pub command_open: bool,
    pub command_label_create_open: bool,
    pub selection_color_open: bool,
}

#[handlers]
impl KeepBoard {
    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let command = handle.clone();
        handle.watch_field::<bool, _>("command_open", move |open, prev| {
            if !*open || prev.copied().unwrap_or(false) {
                return;
            }
            command.update(|board| {
                board.command_label_create_open = false;
            });
            pocopine::store::<KeepStore>().update(|s| s.command_label_query.clear());
        });

        // Global keyboard shortcuts, scope-bound to the KeepBoard
        // mount via `pocopine::on!`: when the board unmounts the
        // listener detaches automatically. Matches Keep's
        // shortcuts — C for a new text note, L for a new todo —
        // and bails out the moment an input / textarea /
        // contenteditable surface is focused so typing isn't
        // hijacked.
        let Some(doc) = pocopine::dom::window().and_then(|w| w.document()) else {
            return;
        };
        pocopine::on!(doc, keydown, |ev| {
            if ev.ctrl_key() || ev.meta_key() || ev.alt_key() {
                return;
            }
            if is_typing_target() {
                return;
            }
            match ev.key().to_lowercase().as_str() {
                "c" => {
                    ev.prevent_default();
                    pocopine::store::<KeepStore>().update(|s| {
                        s.show_notes();
                        s.expand_composer_text();
                    });
                    focus_after_flush(".composer.expanded textarea");
                }
                "l" => {
                    ev.prevent_default();
                    pocopine::store::<KeepStore>().update(|s| {
                        s.show_notes();
                        s.expand_composer_todo();
                    });
                    focus_after_flush(".composer.expanded .cl-add .cl-txt");
                }
                _ => {}
            }
        });
    }

    pub fn on_mount(&mut self) {
        // Belt-and-suspenders: KeepStore::default() seeds section_kind /
        // draft_kind / draft_color, but on_mount fires *after* the first
        // template walk, so re-asserting via the store proxy here
        // guarantees a change notification reaches every pp-if /
        // pp-show subscriber even if the construct-time write didn't.
        pocopine::store::<KeepStore>().update(|s| {
            s.ensure_defaults();
        });
        pocopine::store::<KeepStore>().observe(KeepStore::tag_label_registry, |labels, _| {
            let labels = labels.clone();
            pocopine::store::<KeepStore>().update(move |s| {
                s.remember_labels(labels.clone());
            });
        });
        pocopine::store::<KeepStore>().observe(KeepStore::note_view_signature, |_, _| {
            pocopine::store::<KeepStore>().update(KeepStore::rebuild_visible_notes);
        });

        // Hook sync to the store: notes lives on KeepStore, so the sync
        // collection's selector mutably borrows the store, not the board.
        let plugins = Plugins;
        let Some(client) = plugins.get::<pocopine_sync::SyncClient>() else {
            pocopine::store::<KeepStore>().update(|s| {
                s.notes.set_error("sync plugin not installed");
            });
            return;
        };
        let notes_result = client
            .collection(pocopine::store::<KeepStore>(), |s: &mut KeepStore| {
                &mut s.notes
            })
            .stream(KEEP_STREAM)
            .and_then(|c| c.open());
        let tags_result = client
            .collection(pocopine::store::<KeepStore>(), |s: &mut KeepStore| {
                &mut s.tags
            })
            .stream(KEEP_TAGS_STREAM)
            .and_then(|c| c.open());
        if let Err(err) = notes_result {
            pocopine::store::<KeepStore>().update(|s| {
                s.status = "sync failed".into();
                s.notes.set_error(err.to_string());
            });
        } else if let Err(err) = tags_result {
            pocopine::store::<KeepStore>().update(|s| {
                s.status = "sync failed".into();
                s.notes.set_error(err.to_string());
            });
        }
    }

    pub fn toggle_sidebar(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::toggle_sidebar);
    }

    pub fn show_notes(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::show_notes);
    }

    pub fn show_archive(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::show_archive);
    }

    pub fn show_label(&mut self, label: String) {
        pocopine::store::<KeepStore>().update(move |s| s.show_label(label));
    }

    pub fn clear_search(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::clear_search);
    }

    pub fn open_command(&mut self) {
        self.command_open = true;
        self.command_label_create_open = false;
        pocopine::store::<KeepStore>().update(|s| s.command_label_query.clear());
    }

    pub fn cmd_new_note(&mut self) {
        self.command_open = false;
        self.command_label_create_open = false;
        pocopine::store::<KeepStore>().update(|s| {
            s.show_notes();
            s.expand_composer_text();
        });
        focus_after_flush(".composer.expanded textarea");
    }

    pub fn cmd_new_todo(&mut self) {
        self.command_open = false;
        self.command_label_create_open = false;
        pocopine::store::<KeepStore>().update(|s| {
            s.show_notes();
            s.expand_composer_todo();
        });
        focus_after_flush(".composer.expanded .cl-add .cl-txt");
    }

    pub fn cmd_show_label_create(&mut self) {
        self.command_label_create_open = true;
        focus_after_flush(".cmd-inline-input");
    }

    pub fn cmd_create_label_inline(&mut self) {
        self.command_open = false;
        self.command_label_create_open = false;
        pocopine::store::<KeepStore>().update(KeepStore::create_command_label);
    }

    pub fn cmd_open_note(&mut self, note_id: String) {
        self.command_open = false;
        self.command_label_create_open = false;
        pocopine::store::<KeepStore>().update(move |s| s.open_editor(note_id));
    }

    pub fn refresh(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::refresh);
    }

    pub fn resync(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::resync);
    }

    pub fn toggle_theme(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::toggle_theme);
    }

    pub fn delete_all_notes(&mut self) {
        pocopine::store::<KeepStore>().update(KeepStore::reset);
    }

    pub fn clear_selection(&mut self) {
        self.selection_color_open = false;
        pocopine::store::<KeepStore>().update(KeepStore::clear_selection);
    }

    pub fn pin_selected(&mut self) {
        self.selection_color_open = false;
        pocopine::store::<KeepStore>().update(KeepStore::pin_selected);
    }

    pub fn archive_selected(&mut self) {
        self.selection_color_open = false;
        pocopine::store::<KeepStore>().update(KeepStore::archive_selected);
    }

    pub fn delete_selected(&mut self) {
        self.selection_color_open = false;
        pocopine::store::<KeepStore>().update(KeepStore::delete_selected);
    }

    pub fn pick_selected_color(&mut self, color: String) {
        self.selection_color_open = false;
        pocopine::store::<KeepStore>().update(move |s| s.set_selected_color(color));
    }
}

/// True when the document's currently focused element is something
/// the user is plausibly typing into, so shortcut keys should not
/// fire and steal the keystroke. Host build returns false because
/// there's no DOM to inspect; on_ready short-circuits earlier via
/// the missing `window()`.
fn is_typing_target() -> bool {
    let Some(active) = pocopine::dom::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
    else {
        return false;
    };
    let tag = active.tag_name().to_uppercase();
    if matches!(tag.as_str(), "INPUT" | "TEXTAREA" | "SELECT") {
        return true;
    }
    active
        .get_attribute("contenteditable")
        .map(|v| v != "false")
        .unwrap_or(false)
}
