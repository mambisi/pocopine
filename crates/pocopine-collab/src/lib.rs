//! # pocopine-collab
//!
//! CRDT collaboration for pocopine apps (RFC 073 Part II), built on `yrs` (the
//! Rust port of Yjs). It is a consumer of the `pocopine-realtime` gateway: a
//! collaborative document is a topic, a Yjs binary update is a message, and an
//! editor is a subscriber. This crate owns the *semantics* — the `yrs` document
//! engine, the Yjs sync handshake, and persistence — while the transport
//! (framing, sessions, fan-out) lives in `pocopine-realtime`.
//!
//! ## Layout (the `client-server-modules` convention)
//!
//! The sync **core** is target-agnostic and compiles on wasm32, because under
//! the Option-A design the browser runs `yrs` in Rust (via `pine-richtext-collab`),
//! not Yjs in JS — so both ends speak the same handshake from the same code:
//!
//! - [`protocol`] — the [`CollabMessage`] wire envelope + [`COLLAB_SUBPROTOCOL`].
//! - [`sync`] — [`CollabDocument`], the `yrs` sync engine (state vector / diff /
//!   apply / full update).
//! - [`doc`] — [`CollabDoc`] document identity → the `collab:{doc_hash}` topic.
//! - [`error`] — [`CollabError`].
//!
//! The host-only half lives behind a single `cfg` gate in [`server`]: the
//! realtime [`CollabSync`] handler and the [`CollabStore`] persistence.

pub mod doc;
pub mod error;
pub mod protocol;
pub mod sync;

pub use doc::{CollabAccess, CollabDoc};
pub use error::{CollabError, CollabResult};
pub use protocol::{COLLAB_SUBPROTOCOL, CollabMessage};
pub use sync::CollabDocument;

#[cfg(not(target_arch = "wasm32"))]
pub mod server;
#[cfg(not(target_arch = "wasm32"))]
pub use server::*;
