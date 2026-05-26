//! `<file-browser-upload-panel>` — wraps the Pine upload compound,
//! exposes the dropzone + active queue, and refreshes the
//! [`crate::FileBrowserStore`] file list whenever an upload completes
//! or fails so the row appears (or stays gone) immediately.

use pine::{
    PineProgressIndicator, PineProgressRoot, PineUploadClear, PineUploadDropzone, PineUploadItem,
    PineUploadItemCancel, PineUploadItemRemove, PineUploadItemRetry, PineUploadRoot,
    PineUploadTrigger,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::FileBrowserStore;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserUploadPanel.poco",
    role = "panel",
    display = "contents",
    uses = [
        PineIcon,
        PineProgressRoot,
        PineProgressIndicator,
        PineUploadRoot,
        PineUploadTrigger,
        PineUploadDropzone,
        PineUploadItem,
        PineUploadItemCancel,
        PineUploadItemRetry,
        PineUploadItemRemove,
        PineUploadClear,
    ]
)]
pub struct FileBrowserUploadPanel {}

#[handlers]
impl FileBrowserUploadPanel {
    pub fn upload_complete(&mut self) {
        pocopine::store::<FileBrowserStore>().update(FileBrowserStore::refresh);
    }

    pub fn upload_failed(&mut self) {
        pocopine::store::<FileBrowserStore>().update(FileBrowserStore::refresh);
    }

    pub fn refresh(&mut self) {
        pocopine::store::<FileBrowserStore>().update(FileBrowserStore::refresh);
    }
}
