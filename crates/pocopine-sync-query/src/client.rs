//! `QueryClient` + `QuerySubscription` runtime.
//!
//! The `QueryClient` owns a refcounted registry of `QuerySubscription`
//! values, one per distinct `QueryKey`. Components subscribe via
//! `QueryClient::subscribe(query)` and get back a `QueryHandle<Row>`
//! whose Drop decrements the refcount. When the last handle drops, the
//! subscription's background tasks are signaled stale via `SyncEpoch`
//! and the registry entry is removed.
//!
//! This module deliberately stays runtime-agnostic at the spawn level:
//! `subscribe` builds the subscription and exposes hooks for an external
//! runtime to drive `/open` + `/pull` + replay. The actual wasm/host
//! drivers wire up in subsequent PRs (Phase 6 — offline + live wakeup).

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use pocopine_sync::{ClientMutation, MutationId, SyncRow, SyncStreamName, SYNC_ENDPOINT_PREFIX};
use serde_json::Value;

use crate::mutator::RowChange;
use crate::query::{Query, QueryKey};
use crate::state::{PendingOverlay, QueryState};

/// Untyped subscription trait — needed so the registry can hold
/// subscriptions of any `Row` type behind one map.
trait AnyQuerySubscription: 'static {
    fn stream(&self) -> &SyncStreamName;
    fn as_any(&self) -> &dyn Any;
    fn refcount(&self) -> usize;
    fn bump_refcount(&self) -> usize;
    fn decrement_refcount(&self) -> usize;
}

/// One live subscription against `(stream, params)`.
///
/// Owns its own state, refcount, and (in subsequent PRs) live-wakeup
/// and background-task lifecycle. The state is `Rc<RefCell<...>>` so
/// the routing engine and any reactive views can hold concurrent
/// borrows in a single-threaded runtime.
pub struct QuerySubscription<Row: 'static> {
    pub(crate) query: Query<Row>,
    pub(crate) state: Rc<RefCell<QueryState<Row>>>,
    refcount: Cell<usize>,
    // Background-task lifecycle is owned externally; the runtime hooks
    // into the subscription via `state`. The Drop on `QueryHandle`
    // signals "no more observers" by decrementing the refcount; the
    // QueryClient gc'd entry triggers cleanup.
}

impl<Row: 'static> QuerySubscription<Row> {
    fn new(query: Query<Row>) -> Self {
        Self {
            query,
            state: Rc::new(RefCell::new(QueryState::default())),
            refcount: Cell::new(0),
        }
    }

    /// Borrow the query this subscription serves.
    pub fn query(&self) -> &Query<Row> {
        &self.query
    }

    /// Borrow the per-query state. Callers wanting reactive updates
    /// should observe the state through a [`QueryHandle`].
    pub fn state(&self) -> &Rc<RefCell<QueryState<Row>>> {
        &self.state
    }
}

impl<Row: 'static> AnyQuerySubscription for QuerySubscription<Row> {
    fn stream(&self) -> &SyncStreamName {
        self.query.stream()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn refcount(&self) -> usize {
        self.refcount.get()
    }

    fn bump_refcount(&self) -> usize {
        let next = self.refcount.get().saturating_add(1);
        self.refcount.set(next);
        next
    }

    fn decrement_refcount(&self) -> usize {
        let next = self.refcount.get().saturating_sub(1);
        self.refcount.set(next);
        next
    }
}

/// The client-side registry of live query subscriptions.
///
/// One per `SyncClient`. Owns one entry per distinct `QueryKey`;
/// observe-with-the-same-query reuses the existing entry via refcount.
///
/// `QueryClient` is `!Send + !Sync` by design — it lives in one wasm
/// task per browser tab. Host-side tests can construct one for unit
/// tests but it never crosses thread boundaries.
pub struct QueryClient {
    endpoint: String,
    registry: RefCell<HashMap<QueryKey, Rc<dyn AnyQuerySubscription>>>,
}

impl QueryClient {
    /// Build a fresh client. The default endpoint is the framework's
    /// `/__pocopine/sync/v1` prefix; tests can override.
    pub fn new() -> Self {
        Self::with_endpoint(SYNC_ENDPOINT_PREFIX.to_string())
    }

    /// Build a client targeting a custom endpoint prefix. Useful for
    /// tests against a router mounted at a different path.
    pub fn with_endpoint(endpoint: String) -> Self {
        Self {
            endpoint,
            registry: RefCell::new(HashMap::new()),
        }
    }

    /// The endpoint prefix this client posts against.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Subscribe to a query, returning a refcounted handle. Two
    /// subscribe calls with logically-equal queries return handles
    /// over the same underlying `QuerySubscription`.
    pub fn subscribe<Row>(&self, query: Query<Row>) -> QueryHandle<Row>
    where
        Row: 'static,
    {
        let key = query.key();
        let mut registry = self.registry.borrow_mut();
        let entry = registry.entry(key).or_insert_with(|| {
            let sub = QuerySubscription::new(query.clone());
            Rc::new(sub) as Rc<dyn AnyQuerySubscription>
        });
        entry.bump_refcount();
        // Coerce back to the typed subscription. The registry maps one
        // QueryKey to one Row type (queries built via the macro can
        // only target one row type per stream), so this downcast is
        // always Some() in correct usage.
        let typed: Rc<QuerySubscription<Row>> = downcast_subscription(entry.clone())
            .expect("subscription Row type matches the query's Row type");
        QueryHandle {
            subscription: typed,
            registry: RegistryWeakRef::new(self, key),
        }
    }

    /// Returns the number of distinct subscriptions currently held.
    /// Mostly useful for tests.
    pub fn active_subscription_count(&self) -> usize {
        self.registry.borrow().len()
    }

    /// Returns the current refcount for a query's subscription, or
    /// `None` if no subscription exists for that query.
    pub fn refcount_of<Row: 'static>(&self, query: &Query<Row>) -> Option<usize> {
        self.registry
            .borrow()
            .get(&query.key())
            .map(|sub| sub.refcount())
    }

    /// Route a batch of row changes for `stream` into every matching
    /// subscription's pending overlay.
    ///
    /// This is the heart of the routing engine: for each active
    /// subscription on `stream`, the query's predicate evaluator
    /// decides whether the change applies. Matching subscriptions
    /// get an optimistic upsert / remove; non-matching subscriptions
    /// are unaffected.
    ///
    /// Called from `mutate(...)` (TBD wasm-side) after `apply_local`
    /// produces the optimistic changes, and again from the canonical
    /// reconciliation path after the server response.
    pub fn route_optimistic_changes<Row>(
        &self,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
        wire_mutation: &ClientMutation<Value>,
        changes: &[RowChange<Row>],
    ) where
        Row: Clone + serde::Serialize + 'static,
    {
        let registry = self.registry.borrow();
        for sub in registry.values() {
            if sub.stream() != stream {
                continue;
            }
            let Some(typed): Option<&QuerySubscription<Row>> =
                downcast_ref_subscription(sub.as_any())
            else {
                continue;
            };
            for change in changes {
                match change {
                    RowChange::Upsert(row) => {
                        if typed.query.matches(row) {
                            // Build the optimistic row to overlay. The
                            // row's `RowKey` comes from the
                            // serialization side — here we rely on
                            // the macro / mutator author placing the
                            // key inside the row before calling
                            // route_optimistic_changes.
                            let mut state = typed.state.borrow_mut();
                            let overlay = PendingOverlay {
                                mutation_id: mutation_id.clone(),
                                mutation: wire_mutation.clone(),
                                optimistic_row: row_key_of(row).map(|key| SyncRow {
                                    key,
                                    version: None,
                                    value: row.clone(),
                                    pending: true,
                                    conflict: false,
                                }),
                                conflict: false,
                            };
                            state.push_pending(overlay);
                        }
                    }
                    RowChange::Delete(key) => {
                        // Delete-shaped overlays remove the row from
                        // visibility while the canonical confirmation
                        // is in flight.
                        let mut state = typed.state.borrow_mut();
                        let overlay = PendingOverlay::<Row> {
                            mutation_id: mutation_id.clone(),
                            mutation: wire_mutation.clone(),
                            optimistic_row: None,
                            conflict: false,
                        };
                        state.push_pending(overlay);
                        let _ = key; // key drives the rebase render layer (TBD)
                    }
                }
            }
        }
    }

    /// Apply canonical row changes returned by a server push. Removes
    /// the corresponding pending overlay and upserts into each
    /// matching subscription's canonical row set.
    pub fn route_canonical_changes<Row>(
        &self,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
        canonical: &[SyncRow<Row>],
    ) where
        Row: Clone + 'static,
    {
        let registry = self.registry.borrow();
        for sub in registry.values() {
            if sub.stream() != stream {
                continue;
            }
            let Some(typed): Option<&QuerySubscription<Row>> =
                downcast_ref_subscription(sub.as_any())
            else {
                continue;
            };
            let mut state = typed.state.borrow_mut();
            // Dequeue the optimistic overlay (idempotent if not present).
            let _ = state.remove_pending(mutation_id);
            for row in canonical {
                if typed.query.matches(&row.value) {
                    state.upsert_canonical(row.clone());
                } else {
                    // Row no longer matches this query's predicate
                    // (e.g. a workspace transition). Remove from
                    // canonical too.
                    state.remove_canonical(&row.key);
                }
            }
        }
    }

    /// Drop a subscription by key when its refcount has reached zero.
    /// Called by [`QueryHandle::drop`] via the back-pointer.
    fn release(&self, key: QueryKey) {
        let mut registry = self.registry.borrow_mut();
        let should_remove = registry
            .get(&key)
            .map(|sub| sub.decrement_refcount() == 0)
            .unwrap_or(false);
        if should_remove {
            registry.remove(&key);
        }
    }
}

impl Default for QueryClient {
    fn default() -> Self {
        Self::new()
    }
}

/// Borrow-form downcast helper used by the routing engine. Returns
/// `None` when the registry entry holds a different `Row` type than
/// the caller expects.
fn downcast_ref_subscription<Row: 'static>(any_ref: &dyn Any) -> Option<&QuerySubscription<Row>> {
    any_ref.downcast_ref::<QuerySubscription<Row>>()
}

/// Best-effort row-key extractor. For now we look for a `id` field
/// in the row's serialization and use that as the `RowKey`. A future
/// PR will replace this with a `Row: HasRowKey` trait so the routing
/// engine doesn't depend on serialization.
fn row_key_of<Row>(row: &Row) -> Option<pocopine_sync::RowKey>
where
    Row: serde::Serialize,
{
    let value = serde_json::to_value(row).ok()?;
    let id = value.get("id")?;
    let id_str = match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    pocopine_sync::RowKey::new(id_str).ok()
}

/// Downcast helper. Returns `None` when the registry entry is held
/// against a different `Row` type than the caller expects — should
/// never happen in correct usage (one stream → one Row type).
fn downcast_subscription<Row: 'static>(
    sub: Rc<dyn AnyQuerySubscription>,
) -> Option<Rc<QuerySubscription<Row>>> {
    // Rc<dyn Trait> → Rc<ConcreteType> needs an unsafe transmute on
    // stable, or we can route through `as_any`. We use `as_any` for
    // safety; the cost is one virtual call at downcast time.
    if sub.as_any().is::<QuerySubscription<Row>>() {
        // SAFETY: `as_any().is::<T>()` returned true, so the concrete
        // type matches. The `Rc` already owns the allocation; we
        // re-typed the pointer.
        let raw = Rc::into_raw(sub) as *const QuerySubscription<Row>;
        // SAFETY: same as above; the layout matches.
        Some(unsafe { Rc::from_raw(raw) })
    } else {
        None
    }
}

/// Handle to a live query subscription. Drop to decrement the
/// refcount; the underlying `QuerySubscription` is gc'd when no
/// handles remain.
pub struct QueryHandle<Row: 'static> {
    subscription: Rc<QuerySubscription<Row>>,
    registry: RegistryWeakRef,
}

impl<Row: 'static> QueryHandle<Row> {
    /// Borrow the underlying query.
    pub fn query(&self) -> &Query<Row> {
        self.subscription.query()
    }

    /// Borrow the per-query state. Hold a borrow only as long as
    /// needed — the routing engine writes through the same `RefCell`.
    pub fn state(&self) -> std::cell::Ref<'_, QueryState<Row>> {
        self.subscription.state.borrow()
    }

    /// Subscription's current refcount. Mostly useful for tests.
    pub fn refcount(&self) -> usize {
        self.subscription.refcount()
    }
}

impl<Row: 'static> Drop for QueryHandle<Row> {
    fn drop(&mut self) {
        self.registry.release();
    }
}

/// Internal helper so [`QueryHandle::drop`] can reach back into the
/// registry to decrement the refcount + remove the entry. Uses a raw
/// pointer (not `Weak`) because `QueryClient` isn't held by `Rc`
/// itself — it's owned by the app's `SyncClient` and stable across
/// the subscription's lifetime.
struct RegistryWeakRef {
    client: *const QueryClient,
    key: QueryKey,
}

impl RegistryWeakRef {
    fn new(client: &QueryClient, key: QueryKey) -> Self {
        Self {
            client: client as *const QueryClient,
            key,
        }
    }

    fn release(&self) {
        // SAFETY: `QueryClient` outlives all `QueryHandle`s it issues
        // (handles can't outlive the client because the client owns
        // the registry that holds the refcounted subscriptions). The
        // pointer is non-null and points at a valid `QueryClient` for
        // the duration of the handle.
        unsafe { &*self.client }.release(self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issues_query(workspace: &str) -> Query<()> {
        Query::builder(SyncStreamName::new("issues").unwrap())
            .raw_param("workspace_id", serde_json::json!(workspace))
            .build()
    }

    #[test]
    fn fresh_client_has_no_subscriptions() {
        let client = QueryClient::new();
        assert_eq!(client.active_subscription_count(), 0);
    }

    #[test]
    fn subscribe_registers_and_returns_handle() {
        let client = QueryClient::new();
        let q = issues_query("W1");
        let handle = client.subscribe::<()>(q.clone());
        assert_eq!(client.active_subscription_count(), 1);
        assert_eq!(handle.refcount(), 1);
        assert_eq!(client.refcount_of(&q), Some(1));
    }

    #[test]
    fn two_handles_to_same_query_share_subscription() {
        let client = QueryClient::new();
        let q = issues_query("W1");
        let h1 = client.subscribe::<()>(q.clone());
        let h2 = client.subscribe::<()>(q.clone());
        assert_eq!(client.active_subscription_count(), 1);
        assert_eq!(h1.refcount(), 2);
        assert_eq!(h2.refcount(), 2);
    }

    #[test]
    fn distinct_queries_get_distinct_subscriptions() {
        let client = QueryClient::new();
        let q1 = issues_query("W1");
        let q2 = issues_query("W2");
        let _h1 = client.subscribe::<()>(q1);
        let _h2 = client.subscribe::<()>(q2);
        assert_eq!(client.active_subscription_count(), 2);
    }

    #[test]
    fn drop_last_handle_removes_subscription() {
        let client = QueryClient::new();
        let q = issues_query("W1");
        let h1 = client.subscribe::<()>(q.clone());
        let h2 = client.subscribe::<()>(q.clone());
        drop(h1);
        assert_eq!(client.active_subscription_count(), 1);
        assert_eq!(client.refcount_of(&q), Some(1));
        drop(h2);
        assert_eq!(client.active_subscription_count(), 0);
        assert_eq!(client.refcount_of(&q), None);
    }

    #[test]
    fn handle_borrows_state_through_refcell() {
        let client = QueryClient::new();
        let q = issues_query("W1");
        let h = client.subscribe::<()>(q);
        let state = h.state();
        assert_eq!(state.canonical_len(), 0);
        assert!(state.pending().is_empty());
    }
}
