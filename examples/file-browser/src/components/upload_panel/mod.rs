//! `<file-browser-upload-panel>` — path header and folder/upload actions.

use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{components::upload_dock::request_upload_picker, StorageBrowserStore};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserUploadPanel.poco",
    role = "panel",
    display = "contents",
    uses = [PineIcon]
)]
pub struct FileBrowserUploadPanel {}

#[handlers]
impl FileBrowserUploadPanel {
    pub fn refresh(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::refresh);
    }

    pub fn open_upload_dock(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::open_upload_dock);
        request_upload_picker();
    }

    pub fn open_new_folder_dialog(&mut self) {
        pocopine::store::<StorageBrowserStore>()
            .update(StorageBrowserStore::open_new_folder_dialog);
    }

    pub fn open_prefix(&mut self, prefix: String) {
        pocopine::store::<StorageBrowserStore>().update(move |s| s.open_prefix(prefix));
    }
}
