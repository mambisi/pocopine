//! `<file-browser-app>` — the only component mounted from
//! `index.html`. It composes the shell and loads saved storage
//! connections on mount.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::StorageBrowserStore;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "FileBrowserApp.poco", role = "panel", display = "contents")]
pub struct FileBrowserApp {}

#[handlers]
impl FileBrowserApp {
    pub fn on_mount(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(|store| {
            store.load_app_config();
            store.load_connections();
        });
    }
}
