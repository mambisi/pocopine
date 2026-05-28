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

use pocopine_sync::{ClientMutation, MutationId, RowKey, SyncResult, SyncStreamName};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

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

// ─── replay scaffolding (RFC 088 §A.5) ────────────────────────────
//
// `PendingOverlay::mutation` carries a wire `ClientMutation<Value>`
// but NOT the `apply_remote` closure (closures aren't
// `Serialize`-able). On hydrate we recover the typed replay path by
// looking up the registered `Mutator` impl through a `MutatorRegistry`
// keyed by `(STREAM, NAME)` and replaying via [`AnyMutator`].

/// Re-export name used inside this module so the persisted-payload
/// envelope key is centralized.
pub(crate) const PERSISTED_MUTATOR_KEY: &str = "__mutator";
/// Re-export name used inside this module so the persisted-payload
/// envelope key is centralized.
pub(crate) const PERSISTED_PAYLOAD_KEY: &str = "__payload";

/// Wrap the wire payload + mutator NAME into a persistence envelope.
///
/// The server NEVER sees this shape: the wire push uses the live
/// `ClientMutation<Value>` directly. The wrap happens at
/// `enqueue_pending_mutation`-time and is unwrapped at hydrate-time
/// before invoking `Mutator::apply_remote`. Returns a JSON object
/// shaped `{ "__mutator": "<NAME>", "__payload": <original> }`.
pub fn wrap_persisted_payload(mutator_name: &str, payload: Value) -> Value {
    serde_json::json!({
        PERSISTED_MUTATOR_KEY: mutator_name,
        PERSISTED_PAYLOAD_KEY: payload,
    })
}

/// Unwrap a persisted envelope back into `(mutator_name, payload)`.
///
/// Returns `None` if the value isn't shaped as a persisted envelope
/// (caller's responsibility to decide whether that's a legacy entry
/// to drop or a corrupt one to log + skip).
pub fn unwrap_persisted_payload(value: &Value) -> Option<(String, Value)> {
    let obj = value.as_object()?;
    let name = obj.get(PERSISTED_MUTATOR_KEY)?.as_str()?.to_string();
    let payload = obj.get(PERSISTED_PAYLOAD_KEY).cloned()?;
    Some((name, payload))
}

/// Outcome of one hydrated replay attempt. Distinct from
/// `crate::driver::ReplayOutcome` because the hydrate-replay path
/// doesn't currently distinguish StillOffline vs Accepted at the
/// caller (the driver loop will retry on the next tick regardless).
#[derive(Debug)]
pub enum HydrateReplayOutcome {
    /// Server accepted the replay; canonical reconcile ran.
    Accepted,
    /// Network error; the pending overlay stays for the next tick.
    StillOffline,
    /// Server rejected, payload was malformed, or the mutator's
    /// registered NAME no longer matches. Caller drops the pending
    /// overlay.
    Rejected(String),
}

/// Type-erased mutator entry registered with a [`QueryClient`].
///
/// One per concrete `Mutator` impl. Lets the hydrate path replay a
/// persisted `ClientMutation<Value>` without monomorphizing the
/// driver against every mutator type.
///
/// `replay_hydrated` is async and `!Send` to match the rest of the
/// sync-query runtime (the driver runs on a single thread per RFC 087).
pub trait AnyMutator: 'static {
    /// Constant mutator NAME — used as the lookup key.
    fn name(&self) -> &'static str;
    /// Constant `STREAM` the mutator's row changes route to.
    fn stream(&self) -> &'static str;
    /// Replay a persisted wire payload by calling the underlying
    /// `Mutator::apply_remote`, then routing the canonical changes
    /// into matching subscriptions. The boxed return is `LocalBoxFuture`
    /// because the runtime is `!Send`.
    ///
    /// `client_inner` is the strong `Rc<QueryClientInner>` the
    /// driver upgraded from its `Weak`. Routing the canonical
    /// changes happens through the public `QueryClient::route_*`
    /// surface — this trait deliberately doesn't depend on the
    /// `QueryClientInner` private layout.
    fn replay_hydrated<'a>(
        &'a self,
        client: &'a crate::QueryClient,
        stream: SyncStreamName,
        mutation: ClientMutation<Value>,
        push_url: String,
    ) -> futures::future::LocalBoxFuture<'a, HydrateReplayOutcome>;
}

/// Concrete carrier of one `Mutator` impl behind the [`AnyMutator`]
/// trait. Stored in the [`crate::QueryClient`]'s registry; one per
/// distinct `M: Mutator`.
pub struct MutatorEntry<M: Mutator> {
    _marker: std::marker::PhantomData<fn() -> M>,
}

impl<M: Mutator> MutatorEntry<M> {
    /// Build an entry. The phantom-data keeps the type without
    /// adding any runtime state.
    pub fn new() -> Self {
        Self {
            _marker: std::marker::PhantomData,
        }
    }
}

impl<M: Mutator> Default for MutatorEntry<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> AnyMutator for MutatorEntry<M>
where
    M: Mutator,
    M::Row: Clone + Serialize + 'static,
{
    fn name(&self) -> &'static str {
        M::NAME
    }

    fn stream(&self) -> &'static str {
        M::STREAM
    }

    fn replay_hydrated<'a>(
        &'a self,
        client: &'a crate::QueryClient,
        stream: SyncStreamName,
        mutation: ClientMutation<Value>,
        push_url: String,
    ) -> futures::future::LocalBoxFuture<'a, HydrateReplayOutcome> {
        Box::pin(async move {
            // Recover the typed payload from the persisted wire envelope.
            // The payload field carries the wrap_persisted_payload
            // envelope at persistence time. If it doesn't (legacy
            // entry, manually-injected payload, etc.) fall back to
            // treating the raw field as the payload.
            let raw_payload = mutation.payload.clone();
            let inner_payload = match unwrap_persisted_payload(&raw_payload) {
                Some((_, p)) => p,
                None => raw_payload,
            };
            let payload: M::Payload = match serde_json::from_value(inner_payload) {
                Ok(p) => p,
                Err(err) => {
                    return HydrateReplayOutcome::Rejected(format!(
                        "hydrated replay payload decode failed: {err}"
                    ));
                }
            };
            // Surface the persisted mutation id through a
            // `MutatorRemoteContext` so the server-side dedup log
            // recognizes the retry as the same logical write.
            struct ReplayCtx {
                mutation_id: MutationId,
                push_url: String,
            }
            impl MutatorRemoteContext for ReplayCtx {
                fn push_url(&self) -> &str {
                    &self.push_url
                }
                fn next_mutation_id(&self) -> SyncResult<MutationId> {
                    Ok(self.mutation_id.clone())
                }
            }
            let ctx = ReplayCtx {
                mutation_id: mutation.id.clone(),
                push_url,
            };
            match M::apply_remote(&ctx, payload).await {
                Ok(canonical) => {
                    crate::client::route_canonical_changes_from_replay::<M::Row>(
                        client,
                        &stream,
                        &mutation.id,
                        &canonical,
                    );
                    HydrateReplayOutcome::Accepted
                }
                Err(err) if err.is_transport() => HydrateReplayOutcome::StillOffline,
                Err(err) => {
                    // Codex P2: dequeue the pending overlay using
                    // the MUTATOR'S row type before returning
                    // Rejected. The outer replay loop in
                    // driver.rs uses the driver's monomorphized
                    // Row type to dequeue, which would skip
                    // subscriptions of a different row type that
                    // received this same optimistic overlay
                    // (cross-mutator routing fan-out). Doing the
                    // cleanup here pins it to M::Row.
                    crate::client::dequeue_pending_for_mutator_row::<M::Row>(
                        client,
                        &stream,
                        &mutation.id,
                    );
                    HydrateReplayOutcome::Rejected(err.to_string())
                }
            }
        })
    }
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
