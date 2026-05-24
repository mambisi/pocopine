//! SQLite local-store backend for `pocopine-sync`.
//!
//! This is an explicit sync extension crate. It is not re-exported from
//! `pocopine` core. The crate owns the SQLite schema and local-store
//! semantics. Native builds use `rusqlite`; browser builds use SQLite WASM
//! with an OPFS worker through the same `SyncLocalStore` API.

mod schema;

pub use schema::{
    BOOTSTRAP_SQL, CLEAR_ROW_CONFLICT_SQL, DELETE_MUTATION_SQL, DELETE_ROW_SQL,
    DELETE_STREAM_ROWS_SQL, META_DEVICE_ID, META_NEXT_MUTATION_COUNTER, META_SCHEMA_VERSION,
    META_TABLE, MUTATIONS_TABLE, ROWS_TABLE, SCHEMA_VERSION, SELECT_PENDING_MUTATIONS_SQL,
    SELECT_ROWS_SQL, SELECT_STREAM_SQL, STREAMS_TABLE, UPDATE_ROW_CONFLICT_SQL,
    UPSERT_MUTATION_SQL, UPSERT_ROW_SQL, UPSERT_STREAM_SQL,
};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod wasm;

#[cfg(not(target_arch = "wasm32"))]
pub use native::SqliteLocalStore;
#[cfg(target_arch = "wasm32")]
pub use wasm::SqliteLocalStore;
