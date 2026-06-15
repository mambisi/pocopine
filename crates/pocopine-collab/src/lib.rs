//! # pocopine-collab
//!
//! Server-side CRDT collaboration for pocopine apps (RFC 073 Part II), built on
//! `yrs` (the Rust port of Yjs). It is a consumer of the `pocopine-realtime`
//! gateway: a collaborative document is a topic, a Yjs binary update is a
//! message, and an editor is a subscriber. This crate owns the *semantics* —
//! the `yrs` document engine, the Yjs sync handshake, and persistence — while
//! the transport (framing, sessions, fan-out) lives in `pocopine-realtime`.
//!
//! Host-only: the `yrs` engine and sync handshake run server-side (browsers
//! speak Yjs in JS, not this crate). All code lives under a single
//! `#[cfg(not(target_arch = "wasm32"))]` gate (the `client-server-modules`
//! convention); on wasm32 this crate is empty.

#[cfg(not(target_arch = "wasm32"))]
pub mod server;

#[cfg(not(target_arch = "wasm32"))]
pub use server::*;
