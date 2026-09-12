//! `<file-browser-file-list>` — virtual-folder/object table.

use pine::{PineToggleGroupItem, PineToggleGroupRoot};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::StorageBrowserStore;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserFileList.poco",
    role = "panel",
    display = "contents",
    uses = [PineIcon, PineToggleGroupRoot, PineToggleGroupItem]
)]
pub struct FileBrowserFileList {}

#[handlers]
impl FileBrowserFileList {
    pub fn select_view(&mut self, event: web_sys::CustomEvent) {
        if let Some(view) = event.detail().as_string() {
            pocopine::store::<StorageBrowserStore>().update(move |s| s.set_entry_view(view));
        }
    }

    pub fn open_prefix(&mut self, prefix: String) {
        pocopine::store::<StorageBrowserStore>().update(move |s| s.open_prefix(prefix));
    }

    /// Row click: folders navigate into the prefix, objects open the
    /// detail panel.
    pub fn open_entry(&mut self, kind: String, prefix: String, key: String) {
        pocopine::store::<StorageBrowserStore>().update(move |s| {
            if kind == "folder" {
                s.open_prefix(prefix);
            } else {
                s.open_object_detail(key);
            }
        });
    }

    pub fn go_up(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::go_up);
    }
}
