//! Route synchronizer for the storage browser shell.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::StorageBrowserStore;

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "FileBrowserRoute.poco",
    role = "panel",
    display = "contents"
)]
pub struct FileBrowserRoute {
    #[prop]
    pub connection_id: String,
    #[prop]
    pub prefix: String,
}

impl RouteComponent for FileBrowserRoute {}

#[handlers]
impl FileBrowserRoute {
    pub fn on_mount(&mut self) {
        self.sync();
    }

    #[watch(connection_id)]
    fn on_connection_change(&mut self, _next: String, _prev: Option<String>) {
        self.sync();
    }

    #[watch(prefix)]
    fn on_prefix_change(&mut self, _next: String, _prev: Option<String>) {
        self.sync();
    }

    fn sync(&mut self) {
        let connection_id = self.connection_id.clone();
        let prefix = self.prefix.clone();
        pocopine::store::<StorageBrowserStore>()
            .update(move |store| store.sync_route(connection_id, prefix));
    }
}
