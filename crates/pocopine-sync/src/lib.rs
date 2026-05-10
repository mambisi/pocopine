//! Sync protocol and plugin extension for Pocopine apps.
//!
//! `pocopine-sync` is an explicit extension crate. It is not re-exported
//! from `pocopine` and it does not add feature flags to the framework core.
//! Apps opt in by depending on this crate, registering the wasm app plugin,
//! and installing the host server plugin.

mod error;
mod protocol;
mod state;

mod client;
#[cfg(not(target_arch = "wasm32"))]
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod server;

pub use error::{SyncError, SyncResult};
pub use protocol::{
    sync_shape_tag, ClientMutation, MutationId, RowKey, RowVersion, SyncChange, SyncCollectionName,
    SyncConflict, SyncCursor, SyncOp, SyncOpenRequest, SyncOpenResponse, SyncOpenShape,
    SyncPullMode, SyncPullRequest, SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncRow,
    SyncShapeName, SYNC_ENDPOINT_PREFIX, SYNC_OPEN_PATH, SYNC_PROTOCOL_V1, SYNC_PULL_PATH,
    SYNC_PUSH_PATH,
};
pub use state::{CollectionState, SyncReason, SyncRequest};

pub use client::{sync_plugin, SyncClient, SyncClientPlugin, SyncCollection};

#[cfg(not(target_arch = "wasm32"))]
pub use memory::{MemorySyncShape, MemorySyncState};

#[cfg(not(target_arch = "wasm32"))]
pub use server::{
    sync_server_plugin, SyncBoxFuture, SyncServer, SyncServerBuilder, SyncServerPlugin,
    SyncShapeSource,
};
