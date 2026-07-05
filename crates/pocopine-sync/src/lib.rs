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
mod state;

#[cfg(not(target_arch = "wasm32"))]
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod server;

pub use error::{SyncError, SyncResult};
pub use local_memory::MemoryLocalStore;
pub use local_store::{
    LocalChangeBatch, LocalPendingMutation, LocalPushResult, LocalSnapshotBatch,
    LocalStreamSnapshot, MutationIdGenerator, SyncLocalFuture, SyncLocalIdentity, SyncLocalStore,
    generate_sync_device_id,
};
pub use protocol::{
    ClientMutation, ClientMutationDraft, MAX_SYNC_TOKEN_LEN, MigrationOutcome, MutationId,
    PARAMS_HASH_HEX_LEN, RowKey, RowVersion, SYNC_ENDPOINT_PREFIX, SYNC_OPEN_PATH,
    SYNC_PROTOCOL_V1, SYNC_PULL_PATH, SYNC_PUSH_PATH, StreamParams, SyncChange, SyncCollectionName,
    SyncConflict, SyncCursor, SyncDeviceId, SyncOp, SyncOpenRequest, SyncOpenResponse,
    SyncOpenStream, SyncPullMode, SyncPullRequest, SyncPullResponse, SyncPushRequest,
    SyncPushResponse, SyncRejectedMutation, SyncRow, SyncScope, SyncSessionId, SyncStreamName,
    SyncStreamSubscription, SyncTombstone, default_schema_version_one, local_stream_key,
    stream_params_hash, sync_stream_params_tag, sync_stream_tag,
};
pub use state::SyncReason;

#[cfg(not(target_arch = "wasm32"))]
pub use memory::{MemorySyncState, MemorySyncStream};

#[cfg(not(target_arch = "wasm32"))]
pub use server::{
    SyncBoxFuture, SyncGuardFuture, SyncServer, SyncServerBuilder, SyncServerPlugin,
    SyncStreamGuard, SyncStreamSource, sync_server_plugin,
};
