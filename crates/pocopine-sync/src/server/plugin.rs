use pocopine_server::axum::routing::post;
use pocopine_server::{Server, ServerPlugin};

use super::*;
use crate::{SYNC_OPEN_PATH, SYNC_PULL_PATH, SYNC_PUSH_PATH};

/// Server plugin that mounts sync routes and provides [`SyncServer`].
///
/// Installing this plugin exposes `/__pocopine/sync/v1/open`, `/pull`, and
/// `/push` for every registered stream. Streams must be registered as either
/// explicitly public or guarded; there is no implicit public registration.
#[derive(Clone)]
pub struct SyncServerPlugin {
    sync: SyncServer,
}

/// Build a sync server plugin.
pub fn sync_server_plugin(sync: SyncServer) -> SyncServerPlugin {
    SyncServerPlugin { sync }
}

impl ServerPlugin for SyncServerPlugin {
    fn name(&self) -> &'static str {
        "pocopine-sync"
    }

    fn install(self, server: Server) -> Server {
        let sync = self.sync;
        server
            .provide_plugin(sync.clone())
            .route(SYNC_OPEN_PATH, post(open_handler).with_state(sync.clone()))
            .route(SYNC_PULL_PATH, post(pull_handler).with_state(sync.clone()))
            .route(SYNC_PUSH_PATH, post(push_handler).with_state(sync))
    }
}
