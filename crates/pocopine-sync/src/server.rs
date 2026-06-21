//! Host-side sync server: stream sources, guards, the HTTP route handlers,
//! and the axum plugin that wires them together.
//!
//! This module is a slim coordinator. The implementation lives in same-named
//! sibling files:
//!
//! * [`source`] — the [`SyncStreamSource`] wire contract + future aliases.
//! * [`guard`] — the [`SyncStreamGuard`] trait and its impls.
//! * [`registration`] — [`SyncServer`], its builder, and the registry types.
//! * [`invalidation`] — RFC 088 §C live wake-up publish methods.
//! * [`handlers`] — the open/pull/push route handlers and helpers.
//! * [`plugin`] — [`SyncServerPlugin`] + axum route wiring.

mod guard;
mod handlers;
mod invalidation;
mod plugin;
mod registration;
mod source;

#[cfg(test)]
mod tests;

// Public surface re-exported at crate root via `lib.rs`.
pub use guard::SyncStreamGuard;
pub use plugin::{SyncServerPlugin, sync_server_plugin};
pub use registration::{SyncServer, SyncServerBuilder};
pub use source::{SyncBoxFuture, SyncGuardFuture, SyncStreamSource};

// Cross-child internals: re-exported at `pub(crate)` so siblings reach them
// via the parent's `use super::*;` and original `server::Item` paths stay
// valid.
pub(crate) use guard::PredicateStreamGuard;
pub(crate) use handlers::{open_handler, pull_handler, push_handler};
// Only the test module references `server_error` across a module boundary.
#[cfg(test)]
pub(crate) use handlers::server_error;
