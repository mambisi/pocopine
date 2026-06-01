//! Google Cloud Storage backend adapter for `pocopine-storage`.
//!
//! This crate implements Pocopine's current bounded sequential proxy upload
//! contract using the official `google-cloud-storage` clients. Completed
//! objects are written by `SafeObjectKey`, while upload-session metadata and
//! component objects live under an internal prefix in the same bucket.
//!
//! The adapter flushes each block once as a component object and assembles the
//! final object with a single `ComposeObject`; only an unflushed tail is
//! buffered inline in the session metadata. It keeps only in-process per-session
//! locks, so route a given upload session to one server replica before using it
//! for horizontally written uploads.

mod control;
mod layout;
mod state;
mod storage;
mod util;

pub use storage::GcsStorageBackend;
