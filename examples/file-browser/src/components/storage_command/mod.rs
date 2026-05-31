//! `<file-browser-storage-command>` — storage object command palettes.

use pine::{
    PineCommandContent, PineCommandEmpty, PineCommandInput, PineCommandItem, PineCommandList,
    PineCommandOverlay, PineCommandPortal, PineCommandRoot,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{StorageBrowserStore, StorageCommandEntry};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserStorageCommand.poco",
    role = "panel",
    display = "contents",
    uses = [
        PineIcon,
        PineCommandRoot,
        PineCommandPortal,
        PineCommandOverlay,
        PineCommandContent,
        PineCommandInput,
        PineCommandList,
        PineCommandItem,
        PineCommandEmpty,
    ]
)]
pub struct FileBrowserStorageCommand {
    #[model]
    pub open: bool,
    pub entries: Vec<StorageCommandEntry>,
    pub loading: bool,
    pub error: String,
}

#[handlers]
impl FileBrowserStorageCommand {
    #[watch(open)]
    fn on_open(&mut self, open: bool, _prev: Option<bool>) {
        if open {
            self.load_all_command_entries();
        }
    }

    pub fn open_command(&mut self) {
        self.open = true;
    }

    pub fn load_all_command_entries(&mut self) {
        self.loading = true;
        self.error.clear();
        dispatch!(
            crate::list_storage_object_commands(String::new()).await,
            |s, result| {
                s.loading = false;
                match result {
                    Ok(entries) => s.entries = entries,
                    Err(err) => s.error = err.to_string(),
                }
            },
        );
    }

    pub fn open_command_result(
        &mut self,
        connection_id: String,
        prefix: String,
        object_name: String,
    ) {
        self.open = false;
        pocopine::store::<StorageBrowserStore>().update(move |s| {
            s.open_search_result(connection_id, prefix, object_name);
        });
    }
}
