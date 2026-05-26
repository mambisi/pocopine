//! S3-compatible backend adapter for `pocopine-storage`.
//!
//! This crate intentionally implements Pocopine's current sequential proxy
//! upload contract. Completed objects are written to the configured bucket by
//! `SafeObjectKey`, while upload-session metadata and staging bytes live under
//! an internal prefix in the same bucket.
//!
//! For S3-compatible services such as MinIO, build an `aws_sdk_s3::Client`
//! with the provider-specific endpoint/path-style settings and pass it to
//! [`S3StorageBackend::new`].
//!
//! This first adapter is deliberately a bounded sequential proxy backend. It
//! keeps staging bytes in memory while appending/completing and relies on
//! in-process per-session locks, so route a given upload session to one server
//! replica or wait for the future provider-side multipart backend before using
//! it for large or horizontally written uploads.

mod layout;
mod state;
mod storage;
mod util;

pub use storage::S3StorageBackend;
