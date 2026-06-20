//! The browser collab client: the target-agnostic sync driver and its wasm
//! WebSocket connection.
//!
//! [`sync::CollabSyncClient`] is the handshake logic (host-tested); the wasm
//! `CollabConnection` is the I/O shell that drives it over a `pocopine-realtime`
//! `RealtimeClient`.

mod sync;
#[cfg(target_arch = "wasm32")]
pub use sync::{CollabSyncClient, SyncOutcome};

#[cfg(target_arch = "wasm32")]
mod connection;
#[cfg(target_arch = "wasm32")]
pub use connection::{CollabConnection, random_client_id};
