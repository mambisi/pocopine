//! Azure Blob Storage backend adapter for `pocopine-storage`.
//!
//! This crate implements Pocopine's current bounded sequential proxy upload
//! contract using the official `azure_storage_blob` client surface. Completed
//! objects are written by `SafeObjectKey`, while upload-session metadata and
//! staging bytes live under an internal prefix in the same Azure container.
//!
//! The adapter rewrites the staged blob on each append and keeps only
//! in-process per-session locks, so route a given upload session to one server
//! replica or wait for a future provider-side block-blob backend before using
//! it for large or horizontally written uploads.

mod layout;
mod state;
mod storage;
mod util;

pub use storage::AzureBlobStorageBackend;
