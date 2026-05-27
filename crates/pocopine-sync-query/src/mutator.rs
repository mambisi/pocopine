//! `Mutator` — a transactional row-changing function.
//!
//! Where CRUD's `Resource::create/save/remove` are bound to a Resource,
//! a `Mutator` is bound to nothing user-visible: it just produces row
//! changes, and the [`crate::client::QueryClient`] (TBD) routes those
//! changes to every active [`crate::Query`] whose predicate matches
//! each row.
//!
//! This is the heart of the design's "no active subscription" property
//! — a mutation that creates a row in workspace W appears in every
//! observed query whose predicate matches W, and is invisible to
//! every other observed query. Predicate evaluation is the routing key,
//! not the calling context.
//!
//! See RFC 086 §5.3 and `docs/sync-query-design.md` §3.4 for design rationale.

use std::pin::Pin;

use pocopine_sync::{RowKey, SyncResult};
use serde::{de::DeserializeOwned, Serialize};

/// Boxed future returned by [`Mutator::apply_remote`]. Aliased to keep
/// the trait signature legible.
pub type MutatorRemoteFuture<Row> =
    Pin<Box<dyn std::future::Future<Output = SyncResult<Vec<RowChange<Row>>>> + 'static>>;

/// A row-shaped change produced by a [`Mutator`] or returned in a
/// canonical push response.
///
/// The routing engine evaluates each `RowChange` against every active
/// query's predicate:
///
/// * `Upsert(row)` — if the query's predicate matches the row, upsert
///   it into the query's state. If the query previously contained the
///   row but the predicate no longer matches (a "row left this shape"
///   transition), remove it.
/// * `Delete(key)` — remove from every query that contained this key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RowChange<R> {
    Upsert(R),
    Delete(RowKey),
}

impl<R> RowChange<R> {
    /// Map the row payload through `f`, preserving the variant.
    pub fn map<F, U>(self, f: F) -> RowChange<U>
    where
        F: FnOnce(R) -> U,
    {
        match self {
            RowChange::Upsert(r) => RowChange::Upsert(f(r)),
            RowChange::Delete(k) => RowChange::Delete(k),
        }
    }
}

/// Outcome of [`QueryClient::mutate`] returned to the caller after the
/// server has responded.
#[derive(Clone, Debug)]
pub enum MutationOutcome<Row> {
    /// Server accepted the mutation. Carries the canonical row changes
    /// it produced (which the routing engine has already applied to
    /// matching subscriptions' canonical state).
    Accepted(Vec<RowChange<Row>>),
    /// Server rejected the mutation. Optimistic state has been rolled
    /// back on every subscription it was applied to.
    Rejected { reason: String },
    /// Server detected a conflict (base_version mismatch). Optimistic
    /// state stays applied with a `conflict` flag; the canonical rows
    /// returned represent the server's view.
    Conflict { server_rows: Vec<Row> },
}

/// A transactional mutator.
///
/// Both methods are required:
///
/// * [`Mutator::apply_local`] runs synchronously inside the routing
///   engine to produce the optimistic row changes. It MUST be
///   deterministic given the payload.
/// * [`Mutator::apply_remote`] runs against the server (async) and
///   returns the canonical row changes from the source. Typically
///   delegates to a `CrudSource` or similar.
///
/// Mutators don't know about queries. The engine handles routing via
/// each query's predicate evaluator.
///
/// # Future macro support
///
/// A planned `#[mutator]` proc-macro will derive `apply_local` from
/// the payload's structural shape (e.g. for a `Create` mutator with a
/// `Row`-shaped payload, the local apply is `vec![Upsert(payload.into())]`).
/// Until then, mutators are implemented manually.
pub trait Mutator: 'static {
    /// Wire-serializable payload type.
    type Payload: Serialize + DeserializeOwned + Send + 'static;

    /// Row type produced by this mutator.
    type Row: Clone + Send + 'static;

    /// Wire identity. The mutator's name + the client's device id form
    /// the mutation_id used for server-side idempotency.
    const NAME: &'static str;

    /// Stream this mutator's row changes belong to. The routing engine
    /// only considers queries on this stream.
    const STREAM: &'static str;

    /// Application schema version this mutator's payload is encoded
    /// under. The server's `push_handler` routes stale payloads
    /// through `SyncStreamSource::migrate_payload` when this differs
    /// from the source's current version.
    const SCHEMA_VERSION: u32;

    /// Produce optimistic row changes from the payload. Runs inside the
    /// routing engine's `handle.update`; MUST NOT await, MUST be
    /// deterministic, and MUST NOT panic on any valid payload.
    fn apply_local(payload: &Self::Payload) -> Vec<RowChange<Self::Row>>;

    /// Push the payload to the server and return canonical row changes.
    /// Server-side implementations typically delegate to a `CrudSource`
    /// or similar. The framework calls this from the wasm client's
    /// background task; the host side stubs to `SyncError::Unsupported`.
    fn apply_remote(
        client: &dyn MutatorRemoteContext,
        payload: Self::Payload,
    ) -> MutatorRemoteFuture<Self::Row>;
}

/// Context handed to [`Mutator::apply_remote`]. Provides the sync client,
/// endpoint, and auth glue without coupling the mutator to a concrete
/// type.
///
/// The trait is implemented by the runtime; users don't impl it.
///
/// Kept as a trait object so different runtime backends (wasm fetch,
/// host test stub) can plug in without changing the mutator signature.
pub trait MutatorRemoteContext {
    /// The server-side push endpoint URL for this mutator's stream.
    fn push_url(&self) -> &str;

    /// Generate the next mutation id for this device. Used for the
    /// server's idempotency log.
    fn next_mutation_id(&self) -> SyncResult<pocopine_sync::MutationId>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_change_map_preserves_variant() {
        let upsert: RowChange<i32> = RowChange::Upsert(42);
        let mapped = upsert.map(|n| n.to_string());
        assert!(matches!(mapped, RowChange::Upsert(ref s) if s == "42"));

        let delete: RowChange<i32> = RowChange::Delete(RowKey::new("row_1").unwrap());
        let mapped = delete.map(|n: i32| n.to_string());
        assert!(matches!(mapped, RowChange::Delete(ref k) if k.as_str() == "row_1"));
    }
}
