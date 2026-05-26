//! `<file-browser-file-list>` — filter pills, toolbar, table, and
//! empty state. Reads `files` / `loading` / `error` from
//! [`crate::FileBrowserStore`]; the filter pill state lives locally
//! so the `pine-toggle-group` `#[model]` round-trip stays clean.

use pine::{PineToggleGroupItem, PineToggleGroupRoot};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::FileBrowserStore;

#[derive(Serialize, Deserialize)]
#[component(
    template = "FileBrowserFileList.poco",
    role = "panel",
    display = "contents",
    uses = [PineIcon, PineToggleGroupRoot, PineToggleGroupItem]
)]
pub struct FileBrowserFileList {
    /// Active filter pill — `"all" | "documents" | "images" |
    /// "archives"`. Two-way bound to the filter `pine-toggle-group`
    /// via `pp-model:value="view"`.
    #[model]
    pub view: String,
}

impl Default for FileBrowserFileList {
    fn default() -> Self {
        Self {
            view: "all".to_string(),
        }
    }
}

#[handlers]
impl FileBrowserFileList {
    pub fn remove_file(&mut self, file_id: String) {
        pocopine::store::<FileBrowserStore>().update(move |s| s.remove_file(file_id));
    }
}
