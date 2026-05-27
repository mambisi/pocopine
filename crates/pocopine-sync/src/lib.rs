//! Sync protocol and plugin extension for Pocopine apps.
//!
//! `pocopine-sync` is an explicit extension crate. It is not re-exported
//! from `pocopine` and it does not add feature flags to the framework core.
//! Apps opt in by depending on this crate, registering the wasm app plugin,
//! and installing the host server plugin.

mod error;
mod local_memory;
mod local_store;
mod protocol;
mod sign_out;
mod state;

mod client;
#[cfg(not(target_arch = "wasm32"))]
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod server;

pub use error::{SyncError, SyncResult};
pub use local_memory::MemoryLocalStore;
pub use local_store::{
    generate_sync_device_id, LocalChangeBatch, LocalPendingMutation, LocalPushResult,
    LocalSnapshotBatch, LocalStreamSnapshot, MutationIdGenerator, SyncLocalFuture,
    SyncLocalIdentity, SyncLocalStore,
};
pub use protocol::{
    default_schema_version_one, local_stream_key, sync_stream_tag, ClientMutation,
    ClientMutationDraft, MigrationOutcome, MutationId, RowKey, RowVersion, StreamParams, SyncChange,
    SyncCollectionName, SyncConflict, SyncCursor, SyncDeviceId, SyncOp, SyncOpenRequest,
    SyncOpenResponse, SyncOpenStream, SyncPullMode, SyncPullRequest, SyncPullResponse,
    SyncPushRequest, SyncPushResponse, SyncRejectedMutation, SyncRow, SyncSessionId,
    SyncStreamName, SyncStreamSubscription, MAX_SYNC_TOKEN_LEN, SYNC_ENDPOINT_PREFIX,
    SYNC_OPEN_PATH, SYNC_PROTOCOL_V1, SYNC_PULL_PATH, SYNC_PUSH_PATH,
};
pub use state::{CollectionState, PendingMutation, SyncReason, SyncRequest};

pub use client::{
    sync_plugin, CollectionSelector, SyncClient, SyncClientPlugin, SyncCollection,
    SyncLocalStoreHandle,
};
pub use pocopine_core::Handle;
pub use sign_out::SignOutSubscription;

#[cfg(not(target_arch = "wasm32"))]
pub use memory::{MemorySyncState, MemorySyncStream};

#[cfg(not(target_arch = "wasm32"))]
pub use server::{
    sync_server_plugin, SyncBoxFuture, SyncGuardFuture, SyncServer, SyncServerBuilder,
    SyncServerPlugin, SyncStreamGuard, SyncStreamSource,
};
