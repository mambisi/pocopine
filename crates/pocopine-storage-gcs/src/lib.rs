//! Google Cloud Storage backend adapter for `pocopine-storage`.
//!
//! This crate implements Pocopine's current bounded sequential proxy upload
//! contract using the official `google-cloud-storage` clients. Completed
//! objects are written by `SafeObjectKey`, while upload-session metadata and
//! staging bytes live under an internal prefix in the same bucket.
//!
//! The adapter rewrites the staged object on each append and keeps only
//! in-process per-session locks, so route a given upload session to one server
//! replica or wait for a future provider-side multipart backend before using it
//! for large or horizontally written uploads.

mod control;
mod layout;
mod state;
mod storage;
mod util;

pub use storage::GcsStorageBackend;
