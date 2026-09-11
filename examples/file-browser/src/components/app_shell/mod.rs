//! `<file-browser-app>` — the only component mounted from
//! `index.html`. It composes the shell and loads saved storage
//! connections on mount.

use pocopine::events::{self, ev};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use web_sys::KeyboardEvent;

use crate::StorageBrowserStore;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "FileBrowserApp.poco", role = "panel", display = "contents")]
pub struct FileBrowserApp {}

#[handlers]
impl FileBrowserApp {
    fn on_ready(&self) {
        let Some(doc) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        events::on_scoped(&doc, ev::keydown, move |ev: KeyboardEvent| {
            if ev.key().to_ascii_lowercase() == "n"
                && (ev.meta_key() || ev.ctrl_key())
                && !ev.shift_key()
                && !ev.alt_key()
            {
                ev.prevent_default();
                pocopine::store::<StorageBrowserStore>()
                    .update(StorageBrowserStore::open_new_folder_dialog);
            }
        });
    }

    pub fn on_mount(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(|store| {
            store.load_app_config();
            store.load_connections();
        });
    }
}
