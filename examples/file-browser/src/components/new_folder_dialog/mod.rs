//! `<file-browser-new-folder-dialog>` — create a virtual S3 folder.

use pine::{
    PineDialogClose, PineDialogContent, PineDialogDescription, PineDialogOverlay, PineDialogPortal,
    PineDialogRoot, PineDialogTitle,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::StorageBrowserStore;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserNewFolderDialog.poco",
    role = "panel",
    display = "contents",
    uses = [
        PineIcon,
        PineDialogRoot,
        PineDialogPortal,
        PineDialogOverlay,
        PineDialogContent,
        PineDialogTitle,
        PineDialogDescription,
        PineDialogClose,
    ]
)]
pub struct FileBrowserNewFolderDialog {
    #[model]
    pub open: bool,
    pub folder_name: String,
    pub error: String,
    pub creating: bool,
}

#[handlers]
impl FileBrowserNewFolderDialog {
    pub fn close(&mut self) {
        if self.creating {
            return;
        }
        self.open = false;
        self.error.clear();
    }

    pub fn create_folder(&mut self) {
        if self.creating {
            return;
        }
        let folder_name = self.folder_name.trim().to_string();
        if folder_name.is_empty() {
            self.error = "folder name is required".to_string();
            return;
        }
        let (connection_id, parent_prefix) =
            pocopine::store::<StorageBrowserStore>().with(|store| {
                (
                    store.selected_connection_id.clone(),
                    store.current_prefix.clone(),
                )
            });
        if connection_id.is_empty() {
            self.error = "select a storage connection first".to_string();
            return;
        }

        self.creating = true;
        self.error.clear();
        dispatch!(
            crate::create_storage_folder(connection_id.clone(), parent_prefix.clone(), folder_name)
                .await,
            |s, result| {
                s.creating = false;
                match result {
                    Ok(listing) => {
                        s.open = false;
                        s.folder_name.clear();
                        pocopine::store::<StorageBrowserStore>().update(move |store| {
                            store.apply_created_folder_listing(
                                connection_id,
                                parent_prefix,
                                listing,
                            );
                        });
                    }
                    Err(err) => s.error = err.to_string(),
                }
            },
        );
    }
}
