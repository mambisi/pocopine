//! RFC-100 — the asset-bucket client shared by the CLI sync and the
//! Mode B serving proxy.
//!
//! Pocopine's asset pipeline has exactly **one** bucket layout —
//! content-addressed keys `assets/<hash8>/<path>` written with an
//! immutable cache-control header — and this crate is the single
//! client for it. `pocopine assets push` (pocopine-cli) uses
//! [`AssetStore::list_keys`] + [`AssetStore::put`] for the hash-diff
//! sync; the `pocopine-server` `/assets/*` proxy uses
//! [`AssetStore::get`] to serve objects out of a private bucket.
//! Keeping both halves on this one type is what guarantees the
//! write path and the read path can never disagree about keys,
//! content types, or cache headers.
//!
//! The store speaks to any S3-compatible endpoint (Railway Buckets,
//! Cloudflare R2, AWS S3, MinIO) with static credentials; custom
//! endpoints are addressed path-style, which is what R2/MinIO/Railway
//! expect.
//!
//! This is deliberately a **leaf crate** (aws-sdk-s3 + tracing only):
//! `pocopine-server` consumes it, and the storage crates sit above
//! `pocopine-server` in the dependency graph, so the asset client
//! cannot live in `pocopine-storage-s3` without a cycle.

#[cfg(not(target_arch = "wasm32"))]
pub mod server;

#[cfg(not(target_arch = "wasm32"))]
pub use server::*;
