//! Host-side collab implementation (RFC 073 Part II).
//!
//! A single `#[cfg(not(target_arch = "wasm32"))]` gate at the crate root covers
//! everything here; submodules carry no `cfg` (the `client-server-modules`
//! skill).

mod doc;
mod error;
mod handler;
mod protocol;
mod store;
mod sync;

pub use doc::{CollabAccess, CollabDoc};
pub use error::{CollabError, CollabResult};
pub use handler::CollabSync;
pub use protocol::{COLLAB_SUBPROTOCOL, CollabMessage};
pub use store::{CollabSnapshot, CollabStore, MemoryCollabStore};
pub use sync::CollabDocument;
