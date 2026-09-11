//! Route synchronizer for the storage browser shell.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::StorageBrowserStore;

#[derive(Default, Serialize, Deserialize, RouteComponent)]
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

#[handlers]
impl FileBrowserRoute {
    pub fn on_mount(&mut self) {
        // Captured route parameters are seeded before mount; a parameter
        // change remounts this route, so each navigation syncs exactly once.
        self.sync();
    }

    fn sync(&mut self) {
        let connection_id = self.connection_id.clone();
        let prefix = self.prefix.clone();
        pocopine::store::<StorageBrowserStore>()
            .update(move |store| store.sync_route(connection_id, prefix));
    }
}
