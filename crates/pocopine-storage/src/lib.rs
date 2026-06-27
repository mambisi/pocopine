//! Storage protocol, browser client, and server plugin extension for Pocopine.
//!
//! This crate is an explicit extension. Apps opt in by installing the browser
//! app plugin and the host server plugin. Uploads and reads are organized by
//! named scopes; [`StorageObjectScope`] lets apps define one typed contract for
//! the browser scope name, server upload policy, and durable object key layout.

#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod backend_common;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod checksum;
mod client;
mod error;
#[cfg(feature = "image-compression")]
mod image_compression;
mod protocol;

#[cfg(not(target_arch = "wasm32"))]
mod local_fs;
#[cfg(not(target_arch = "wasm32"))]
mod memory;
#[cfg(not(target_arch = "wasm32"))]
mod server;

#[cfg(feature = "image-compression")]
pub use client::ImageUploadBuilder;
#[cfg(target_arch = "wasm32")]
#[doc(hidden)]
pub use client::{BrowserStorageRequest, BrowserStorageResponse, BrowserStorageTransport};
pub use client::{
    ResumableUpload, ResumableUploadBuilder, StorageClient, StorageClientPlugin,
    StorageObjectClient, StorageScopeClient, UploadBuilder, UploadClient, UploadClientPlugin,
    UploadScopeClient, storage_plugin, upload_plugin,
};

#[cfg(all(target_arch = "wasm32", any(test, feature = "test-utils")))]
#[doc(hidden)]
pub use client::{__reset_browser_transport_for_test, __set_browser_transport_for_test};
pub use error::{StorageError, StorageResult};
#[cfg(feature = "image-compression")]
pub use image_compression::{
    CompressedImageVariant, ImageSourceInfo, compress_image_variant, compress_image_variants,
    inspect_image,
};
pub use protocol::{
    AnonymousUploadBinding, BackendCapabilities, ChecksumAlgorithm, ChecksumPolicy, CompleteUpload,
    CompleteUploadRequest, IMAGE_VARIANT_BASE_KEY_METADATA_KEY, IMAGE_VARIANT_HEIGHT_METADATA_KEY,
    IMAGE_VARIANT_METADATA_KEY, IMAGE_VARIANT_ORIGINAL, IMAGE_VARIANT_WIDTH_METADATA_KEY,
    ImageUploadManifest, ImageVariantFormat, ImageVariantObject, ImageVariantPolicy,
    ImageVariantResize, ImageVariantSpec, InitiateUpload, InitiateUploadRequest,
    MAX_STORAGE_TOKEN_LEN, MetadataSchema, ObjectChecksum, ObjectMetadata, ObjectOwnerRef,
    ObjectRef, ObjectVisibility, PartSpec, PrincipalRef, ReadDisposition, ReadUrlRequest,
    STORAGE_ANON_COOKIE, STORAGE_ENDPOINT_PREFIX, STORAGE_OBJECTS_PREFIX, STORAGE_PROTOCOL_V1,
    STORAGE_SCOPES_PREFIX, STORAGE_TUS_ENDPOINT_PREFIX, STORAGE_UPLOADS_PATH,
    STORAGE_UPLOADS_PREFIX, SafeObjectKey, SignedRead, StorageBackendName, StorageKey,
    StorageObjectScope, StorageResponse, TransferPlan, UploadIntent, UploadPhase, UploadPolicy,
    UploadPolicyDescriptor, UploadProgress, UploadSession, UploadSessionId, UploadSessionStatus,
    UploadStrategy, UploadTarget, UploadedPartStatus, UploadedPartView, image_variant_key,
    plan_parts,
};

#[cfg(not(target_arch = "wasm32"))]
pub use local_fs::LocalFsStorageBackend;

#[cfg(not(target_arch = "wasm32"))]
pub use memory::MemoryStorageBackend;

#[cfg(not(target_arch = "wasm32"))]
pub use server::{
    ObjectBody, ObjectRead, StorageActor, StorageBackend, StorageBoxFuture, StorageContext,
    StorageGuardFuture, StorageKeyFuture, StorageKeyResolver, StorageObjectAccess,
    StorageObjectGuard, StorageScope, StorageScopeBuilder, StorageScopeGuard, StorageServer,
    StorageServerBuilder, StorageServerPlugin, StorageTusServerPlugin, UploadBody,
    UploadByteStream, storage_server_plugin, storage_tus_server_plugin,
};
