//! `<file-browser-file-detail>` — the right-column object inspector.
//!
//! Opens when a file row is clicked (`$store.storage.detail_open`). Shows
//! an in-browser preview at the top (image / pdf / video / audio, else a
//! placeholder), an attributes block, and two collapsible sections built
//! on Pine's accordion. All data is read from `$store.storage.detail*`;
//! handlers delegate to the store.

use pine::{
    PineAccordionContent, PineAccordionItem, PineAccordionRoot, PineAccordionTrigger,
    PineDialogClose, PineDialogContent, PineDialogOverlay, PineDialogPortal, PineDialogRoot,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use crate::StorageBrowserStore;

#[derive(Serialize, Deserialize)]
#[component(
    template = "FileBrowserFileDetail.poco",
    role = "panel",
    display = "contents",
    uses = [
        PineIcon,
        PineAccordionRoot,
        PineAccordionItem,
        PineAccordionTrigger,
        PineAccordionContent,
        PineDialogRoot,
        PineDialogPortal,
        PineDialogOverlay,
        PineDialogContent,
        PineDialogClose,
    ]
)]
pub struct FileBrowserFileDetail {
    /// Which accordion sections are open. Defaults to both, matching the
    /// Firebase reference.
    #[model]
    pub sections: Vec<String>,
    /// Whether the fullscreen preview lightbox is open. Two-way bound to
    /// the dialog so Esc / backdrop / close all clear it.
    pub lightbox_open: bool,
    /// The value most recently copied to the clipboard, held for ~1.5s so the
    /// matching copy row swaps its icon to a check (shadcn-style). A copy row
    /// shows the check only while `copied` equals its own value, so the two
    /// rows give independent feedback. Empty = nothing recently copied.
    pub copied: String,
}

impl Default for FileBrowserFileDetail {
    fn default() -> Self {
        Self {
            sections: vec!["location".to_string(), "metadata".to_string()],
            lightbox_open: false,
            copied: String::new(),
        }
    }
}

#[handlers]
impl FileBrowserFileDetail {
    pub fn close_detail(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::close_detail);
    }

    /// Expand the preview card into the fullscreen lightbox.
    pub fn open_lightbox(&mut self) {
        self.lightbox_open = true;
    }

    /// Step to the previous / next object in the current listing.
    pub fn detail_prev(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::detail_prev);
    }

    pub fn detail_next(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::detail_next);
    }

    /// Copy a value (storage location / download URL) to the clipboard and
    /// flash a shadcn-style "copied" check on the matching row, reverting
    /// after ~1.5s. `copied` records *which* value was copied so only that
    /// row shows the check.
    pub fn copy_text(&mut self, text: String) {
        if let Some(clipboard) = web_sys::window().map(|window| window.navigator().clipboard()) {
            let _ = clipboard.write_text(&text);
        }
        self.copied = text;

        // Revert the check back to the copy icon after a beat. The timer fires
        // on a later task, so updating through the handle is safe here.
        let handle = this::<Self>();
        if let Some(window) = web_sys::window() {
            let revert = Closure::once_into_js(move || {
                handle.update(|detail| detail.copied = String::new());
            });
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                revert.unchecked_ref(),
                1500,
            );
        }
    }
}
