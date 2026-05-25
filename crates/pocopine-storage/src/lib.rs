//! Storage protocol, browser client, and server plugin extension for Pocopine.
//!
//! This crate is an explicit extension. Apps opt in by installing the browser
//! app plugin and the host server plugin.

#[cfg(not(target_arch = "wasm32"))]
mod backend_common;
#[cfg(not(target_arch = "wasm32"))]
mod checksum;
mod client;
mod error;
mod protocol;

#[cfg(not(target_arch = "wasm32"))]
mod local_fs;
#[cfg(not(target_arch = "wasm32"))]
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod server;

pub use client::{
    storage_plugin, StorageClient, StorageClientPlugin, StorageScopeClient, UploadBuilder,
};
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub use client::{
    BrowserStorageRequest, BrowserStorageResponse, BrowserStorageTransport,
    __reset_browser_transport_for_test, __set_browser_transport_for_test,
};
pub use error::{StorageError, StorageResult};
pub use protocol::{
    AnonymousUploadBinding, ChecksumAlgorithm, ChecksumPolicy, CompleteUpload,
    CompleteUploadRequest, InitiateUpload, InitiateUploadRequest, MetadataSchema, ObjectChecksum,
    ObjectMetadata, ObjectOwnerRef, ObjectRef, ObjectVisibility, PrincipalRef, SafeObjectKey,
    SignedRead, StorageBackendKind, StorageBackendName, StorageKey, StorageResponse, TransferPlan,
    UploadIntent, UploadPhase, UploadPolicy, UploadPolicyDescriptor, UploadProgress, UploadSession,
    UploadSessionId, UploadSessionStatus, UploadStrategy, UploadTarget, UploadedPartStatus,
    UploadedPartView, MAX_STORAGE_TOKEN_LEN, STORAGE_ANON_COOKIE, STORAGE_ENDPOINT_PREFIX,
    STORAGE_PROTOCOL_V1, STORAGE_SCOPES_PREFIX, STORAGE_UPLOADS_PATH, STORAGE_UPLOADS_PREFIX,
};

#[cfg(not(target_arch = "wasm32"))]
pub use local_fs::LocalFsStorageBackend;

#[cfg(not(target_arch = "wasm32"))]
pub use memory::MemoryStorageBackend;

#[cfg(not(target_arch = "wasm32"))]
pub use server::{
    storage_server_plugin, storage_tus_server_plugin, StorageActor, StorageBackend,
    StorageBoxFuture, StorageContext, StorageGuardFuture, StorageKeyFuture, StorageKeyResolver,
    StorageScope, StorageScopeBuilder, StorageScopeGuard, StorageServer, StorageServerBuilder,
    StorageServerPlugin, StorageTusServerPlugin,
};
