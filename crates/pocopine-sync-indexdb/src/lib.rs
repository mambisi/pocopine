//! IndexedDB-backed browser local store for `pocopine-sync`.
//!
//! This backend is durable across page reloads and works without the
//! cross-origin isolation headers required by OPFS-backed SQLite.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use native::IndexedDbLocalStore;
#[cfg(target_arch = "wasm32")]
pub use wasm::IndexedDbLocalStore;
