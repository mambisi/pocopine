//! `<file-browser-upload-dock>` — floating upload queue.

use pine::{
    PineProgressIndicator, PineProgressRoot, PineUploadClear, PineUploadDropzone, PineUploadItem,
    PineUploadItemCancel, PineUploadItemRemove, PineUploadItemRetry, PineUploadRoot,
    PineUploadTrigger,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::StorageBrowserStore;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use web_sys::{Event, HtmlInputElement};

#[cfg(target_arch = "wasm32")]
const OPEN_UPLOAD_PICKER_EVENT: &str = "file-browser:open-upload-picker";
#[cfg(target_arch = "wasm32")]
const UPLOAD_INPUT_SELECTOR: &str =
    "pine-upload-root[scope=\"storage-browser\"] .pine-upload-input";

pub fn request_upload_picker() {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            return;
        };
        let Ok(event) = Event::new(OPEN_UPLOAD_PICKER_EVENT) else {
            return;
        };
        let _ = document.dispatch_event(&event);
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserUploadDock.poco",
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
pub struct FileBrowserUploadDock {}

#[handlers]
impl FileBrowserUploadDock {
    pub fn on_ready(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            let Some(document) = web_sys::window().and_then(|window| window.document()) else {
                return;
            };
            let picker_document = document.clone();
            pocopine::events::on_named_scoped::<Event, _, _>(
                &document,
                OPEN_UPLOAD_PICKER_EVENT,
                move |_event| {
                    pocopine::store::<StorageBrowserStore>()
                        .update(StorageBrowserStore::expand_upload_dock);
                    let Ok(Some(input)) = picker_document.query_selector(UPLOAD_INPUT_SELECTOR)
                    else {
                        return;
                    };
                    if let Ok(input) = input.dyn_into::<HtmlInputElement>() {
                        input.click();
                    }
                },
            );
        }
    }

    pub fn open(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::open_upload_dock);
    }

    pub fn expand(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::expand_upload_dock);
    }

    pub fn expand_and_pick(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::expand_upload_dock);
        request_upload_picker();
    }

    pub fn collapse(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::collapse_upload_dock);
    }

    pub fn close(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::close_upload_dock);
    }

    pub fn upload_complete(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::refresh);
    }

    pub fn upload_failed(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::refresh);
    }
}
