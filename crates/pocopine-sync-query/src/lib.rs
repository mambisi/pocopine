//! `pocopine-sync-query` — query-centric local-first data layer.
//!
//! Parallel to [`pocopine-sync-crud`]: where CRUD is **resource-centric**
//! ("one entity type, one logical view, one optimistic state"), Query is
//! **subscription-centric** ("one query, one cache compartment, one
//! reactive view; many queries per entity type").
//!
//! See [RFC 086](../rfcs/rfc-086-sync-query.md) for design rationale and
//! [RFC 090](../rfcs/rfc-090-merge-crud-into-query.md) for the merge that
//! folded `pocopine-sync-crud` into this crate.
//!
//! # Status
//!
//! Phase 3 scaffold. The crate exposes types and traits; the runtime
//! (predicate-routed mutations, refcounted subscription registry,
//! live wakeup integration) is being built out PR-by-PR per the
//! design doc. See [`README.md`](../README.md) for current state.
//!
//! # Quickstart (target API)
//!
//! ```ignore
//! use pocopine_sync_query::{Query, QueryClient, params};
//!
//! let client: &QueryClient = pocopine::query_client();
//!
//! // Subscribe to a filtered view
//! use issues::field;
//! let handle = client.observe(
//!     Issue::query()
//!         .eq(field::workspace_id, w1)
//!         .any_of(field::status, [Status::Open, Status::InProgress])?
//!         .order_by("created_at", Order::Desc)
//!         .limit(50)
//!         .build()
//! );
//!
//! // Run a mutation; the engine routes its row changes to every
//! // subscription whose predicate matches.
//! client.mutate::<create_issue::Mutator>(payload).await?;
//! ```
//!
//! [`pocopine-sync-crud`]: https://docs.rs/pocopine-sync-crud

pub mod client;
pub mod driver;
pub mod mutator;
pub mod params;
pub mod plugin;
pub mod predicate;
pub mod query;
pub mod selector;
// `source` is host-only — the trait threads `pocopine_core::server::RequestContext`
// through every method, and `pocopine-auth` is cfg-gated to non-wasm.
// Browser code doesn't need a Source trait; it only needs the typed
// query DSL + QueryClient runtime, which live in the modules above.
#[cfg(not(target_arch = "wasm32"))]
pub mod source;
pub mod state;
pub mod wire;
// RFC 090 Phase 2a — mutation-lifecycle types. Pure-data items
// (MutationPayload, TypedMutation, WriteResult, etc.) compile on
// every target. Only the MutationLog trait + MemoryMutationLog
// impl carry `pocopine_core::server::RequestContext` and are inner-gated
// to host. The macro-emitted typed write API on wasm reaches
// MutationPayload/TypedMutation through this module.
pub mod write;

// Re-exports of the curated public API.
pub use client::{QueryClient, QueryHandle, QuerySubscription, QueryView, UpdateToken};
pub use driver::{
    DEFAULT_POLL_INTERVAL, DEFAULT_SYNC_ENDPOINT, DriverEpoch, QueryClientConfig,
    live_event_matches_params,
};
pub use mutator::{
    AnyMutator, HydrateReplayOutcome, MutationOutcome, Mutator, MutatorEntry, MutatorRemoteContext,
    MutatorRemoteFuture, RowChange,
};
pub use plugin::{QueryClientPlugin, query_client_plugin};
pub use predicate::{
    FieldContains, FieldEq, FieldInSet, FieldOrder, FieldRange, contains_matches, range_contains,
};
pub use query::{MatchFn, Order, OrderBy, PartitionHashFn, Query, QueryBuilder, QueryKey};
pub use selector::{AnyArgs, AnyTrackable, SelectorId, SelectorView, TrackToken};
pub use state::QueryState;

// RFC 090 Phase 1 — Query-native server source. The crate also
// continues to expose CRUD-shaped registration through
// `pocopine-sync-crud` for back-compat; the `Source` trait is the
// new direction. See `rfcs/rfc-090-merge-crud-into-query.md`.
#[cfg(not(target_arch = "wasm32"))]
pub use source::{
    DEFAULT_SOURCE_SNAPSHOT_ROW_LIMIT, Source, SourceFuture, SourceId, SourceParamsFn,
    SourceResource, SourceResourceBuilder, SourceVersionFn, query_from_pull_request, source,
};

// RFC 090 Phase 2a — mutation lifecycle types at the crate root so
// users write `use pocopine_sync_query::{WriteResult, MutationLog,
// MemoryMutationLog};` without thinking about the internal module
// split. WriteResult/RemoveResult/Conflict were briefly in
// `source.rs` (Phase 1); Phase 2a moves them to their long-term
// home in `write.rs` and re-exports here. CRUD aliases these to
// the old `Crud*` names for back-compat.
// Pure data types — always exported.
pub use write::{
    AcceptedMutation, Conflict, CreatePayload, DeletePayload, DeleteResult, MutationPayload,
    MutationReservation, OptimisticRowFn, TypedMutation, UpdatePayload, WriteResult,
};

// Host-only: the trait + impl depend on `pocopine_core::server::RequestContext`.
#[cfg(not(target_arch = "wasm32"))]
pub use write::{MemoryMutationLog, MemoryScopeFn, MutationLog};

// The macro lives in its own crate (proc-macros must), but downstream
// code follows the same one-stop-shop import path as
// `pocopine-sync-crud`: `use pocopine_sync_query::query_resource;`.
pub use pocopine_sync_query_macros::{query, query_resource};

// Re-export wire-level types from pocopine-sync that the macro emissions
// and runtime touch directly. Users mostly don't see these.
pub use pocopine_sync::{
    MemoryLocalStore, SyncCursor, SyncLocalStore, SyncStreamName, SyncStreamSubscription,
    local_stream_key,
};

/// Macro-implementation surface. **Not** part of the public API — every
/// symbol here exists only because `pocopine-sync-query-macros` will emit
/// code that names it from downstream crates. Stability is pinned to the
/// macro version, NOT semver of this crate.
#[doc(hidden)]
pub mod __private {
    pub use serde_json;
    // Re-export so generated macro code never needs `::serde::*` (which
    // would force every downstream crate to add `serde` as a direct
    // dep — the macro should compile inside a workspace whose only
    // serde access is transitive through `pocopine-sync-query`).
    pub use serde;
    // Re-export so the `#[query_resource]` macro's emitted
    // `row_to_params` impl (RFC 088 §C) can reach `StreamParams`,
    // `SyncResult`, `SyncError`, and `stream_params_hash` through
    // one stable path — the user's crate only needs
    // `pocopine-sync-query` as a direct dep.
    pub use pocopine_sync;
    // Re-export so the `#[query_resource]` macro's emitted
    // `partition_for_topic` (code-review fix #10) can reach
    // `tracing::warn!` through one stable path. The macro emits a
    // one-shot warning when the resource declares no `required`
    // field and §C live routing is therefore disabled.
    pub use tracing;

    /// FNV-1a 64-bit hash. `const fn` so the `#[query]` macro's
    /// generated `const SELECTOR_ID` initializer evaluates it at the
    /// downstream crate's compile time, producing a stable identity
    /// derived from `module_path!() + "::" + fn_name`. Collisions
    /// would require two `#[query]` fns sharing both the same module
    /// path AND the same name — a user-level conflict that rustc
    /// already catches.
    pub const fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        hash
    }
}
