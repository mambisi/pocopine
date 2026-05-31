//! `<file-browser-sidebar>` — saved connections and connection details.

use pine::{
    PineDropdownMenuContent, PineDropdownMenuItem, PineDropdownMenuPortal, PineDropdownMenuRoot,
    PineDropdownMenuSeparator, PineDropdownMenuTrigger,
};
use pine_icons::PineIcon;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::StorageBrowserStore;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserSidebar.poco",
    role = "panel",
    display = "contents",
    uses = [
        PineIcon,
        PineDropdownMenuRoot,
        PineDropdownMenuTrigger,
        PineDropdownMenuPortal,
        PineDropdownMenuContent,
        PineDropdownMenuItem,
        PineDropdownMenuSeparator,
    ]
)]
pub struct FileBrowserSidebar {
    /// Mobile only: whether the connections list is expanded under the
    /// compact connection trigger. Always shown on desktop via CSS.
    pub connections_expanded: bool,
}

#[handlers]
impl FileBrowserSidebar {
    /// Mobile: toggle the collapsible connections list under the trigger.
    pub fn toggle_connections(&mut self) {
        self.connections_expanded = !self.connections_expanded;
    }

    pub fn select_connection(&mut self, connection_id: String) {
        // Collapse the mobile dropdown after picking (no-op on desktop).
        self.connections_expanded = false;
        pocopine::store::<StorageBrowserStore>()
            .update(move |s| s.select_connection(connection_id));
    }

    pub fn delete_connection(&mut self, connection_id: String) {
        pocopine::store::<StorageBrowserStore>()
            .update(move |s| s.delete_connection(connection_id));
    }

    pub fn reconnect_connection(&mut self, connection_id: String) {
        pocopine::store::<StorageBrowserStore>()
            .update(move |s| s.reconnect_connection(connection_id));
    }

    pub fn edit_connection(&mut self, connection_id: String) {
        pocopine::store::<StorageBrowserStore>().update(move |s| s.edit_connection(connection_id));
    }

    pub fn open_connection_modal(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::open_connection_modal);
    }

    pub fn open_config_dialog(&mut self) {
        pocopine::store::<StorageBrowserStore>().update(StorageBrowserStore::open_config_dialog);
    }
}
