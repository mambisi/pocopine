//! In-memory client-side [`SyncLocalStore`](crate::SyncLocalStore).
//!
//! This module is a slim coordinator. The store struct + trait impl live
//! in [`store`], the reset-folding helper in [`changes`], and the test
//! battery is split across the `tests_*` children.

mod changes;
mod store;

pub use store::MemoryLocalStore;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_clear;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_identity;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_pending;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests_snapshot;
