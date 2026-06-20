//! Host-side collab implementation (RFC 073 Part II): the realtime
//! `SubprotocolHandler` and persistence.
//!
//! A single `#[cfg(not(target_arch = "wasm32"))]` gate at the crate root covers
//! everything here; submodules carry no `cfg` (the `client-server-modules`
//! skill). The target-agnostic sync core (protocol, document, doc identity,
//! error) lives at the crate root so the wasm client speaks the same handshake.

mod handler;
#[cfg(feature = "sqlite")]
mod sqlite_store;
mod store;

pub use handler::CollabSync;
#[cfg(feature = "sqlite")]
pub use sqlite_store::SqliteCollabStore;
pub use store::{CollabSnapshot, CollabStore, MemoryCollabStore};
