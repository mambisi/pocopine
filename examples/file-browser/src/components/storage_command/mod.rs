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
    pub fn on_open_changed(&mut self, event: web_sys::CustomEvent) {
        if let Some(open) = event.detail().as_bool() {
            self.set_open(open);
        }
    }

    pub fn open_command(&mut self) {
        self.set_open(true);
    }

    fn set_open(&mut self, open: bool) {
        let opening = open && !self.open;
        self.open = open;
        if opening {
            self.load_all_command_entries();
        }
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
