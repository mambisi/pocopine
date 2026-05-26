//! `<file-browser-app>` — the only component mounted from
//! `index.html`. It composes the four child components and kicks off
//! the initial file refresh on mount.

use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    FileBrowserFileList, FileBrowserSidebar, FileBrowserStore, FileBrowserTopBar,
    FileBrowserUploadPanel,
};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserApp.poco",
    role = "panel",
    display = "contents",
    uses = [
        PineIcon,
        FileBrowserTopBar,
        FileBrowserSidebar,
        FileBrowserUploadPanel,
        FileBrowserFileList,
    ]
)]
pub struct FileBrowserApp {}

#[handlers]
impl FileBrowserApp {
    pub fn on_mount(&mut self) {
        pocopine::store::<FileBrowserStore>().update(FileBrowserStore::refresh);
    }
}
