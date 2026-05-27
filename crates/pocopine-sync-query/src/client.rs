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
use std::rc::{Rc, Weak};

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
/// Boxed update listener. Fires after every state-mutating op on
/// the owning subscription.
type ListenerFn = Box<dyn Fn()>;

/// `(id, callback)` entry in a subscription's listener list. The id
/// is what [`UpdateToken`] keeps so drop can unregister.
type ListenerEntry = (u64, ListenerFn);

pub struct QuerySubscription<Row: 'static> {
    pub(crate) query: Query<Row>,
    pub(crate) state: Rc<RefCell<QueryState<Row>>>,
    refcount: Cell<usize>,
    /// Update listeners. Each listener fires after every state-
    /// mutating operation on this subscription. Listeners that want
    /// finer-grained dependency tracking should consult
    /// `state.version()` and diff externally.
    listeners: RefCell<Vec<ListenerEntry>>,
    /// Monotonic counter for listener ids. The token returned from
    /// [`QueryView::on_update`] holds the id so drop can unregister.
    next_listener_id: Cell<u64>,
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
            listeners: RefCell::new(Vec::new()),
            next_listener_id: Cell::new(0),
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

    /// Register an update listener. Called after every state-mutating
    /// op on this subscription (canonical upsert / remove, optimistic
    /// push / dequeue, schema reset). Returns an id used by
    /// `unregister_listener`.
    pub(crate) fn register_listener<F>(&self, callback: F) -> u64
    where
        F: Fn() + 'static,
    {
        let id = self.next_listener_id.get();
        self.next_listener_id.set(id.wrapping_add(1));
        self.listeners.borrow_mut().push((id, Box::new(callback)));
        id
    }

    /// Drop a previously-registered listener by id.
    pub(crate) fn unregister_listener(&self, id: u64) {
        let mut listeners = self.listeners.borrow_mut();
        listeners.retain(|(lid, _)| *lid != id);
    }

    /// Fire every registered listener. Called by the routing engine
    /// after each batch of state mutations.
    fn notify_listeners(&self) {
        let listeners = self.listeners.borrow();
        for (_, cb) in listeners.iter() {
            cb();
        }
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
/// Shared interior of [`QueryClient`]. Held by the client itself
/// (strong `Rc`) and by every issued [`QueryHandle`] (`Weak`). A
/// handle that outlives the client sees `Weak::upgrade` → `None`
/// and silently no-ops its refcount decrement instead of triggering
/// a use-after-free.
struct QueryClientInner {
    endpoint: String,
    registry: RefCell<HashMap<QueryKey, Rc<dyn AnyQuerySubscription>>>,
}

pub struct QueryClient {
    inner: Rc<QueryClientInner>,
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
            inner: Rc::new(QueryClientInner {
                endpoint,
                registry: RefCell::new(HashMap::new()),
            }),
        }
    }

    /// The endpoint prefix this client posts against.
    pub fn endpoint(&self) -> &str {
        &self.inner.endpoint
    }

    /// Subscribe to a query, returning a refcounted handle. Two
    /// subscribe calls with logically-equal queries return handles
    /// over the same underlying `QuerySubscription`.
    pub fn subscribe<Row>(&self, query: Query<Row>) -> QueryHandle<Row>
    where
        Row: 'static,
    {
        let key = query.key();
        let mut registry = self.inner.registry.borrow_mut();
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
            registry: RegistryWeakRef::new(&self.inner, key),
        }
    }

    /// Returns the number of distinct subscriptions currently held.
    /// Mostly useful for tests.
    pub fn active_subscription_count(&self) -> usize {
        self.inner.registry.borrow().len()
    }

    /// Returns the current refcount for a query's subscription, or
    /// `None` if no subscription exists for that query.
    pub fn refcount_of<Row: 'static>(&self, query: &Query<Row>) -> Option<usize> {
        self.inner
            .registry
            .borrow()
            .get(&query.key())
            .map(|sub| sub.refcount())
    }

    /// Subscribe to a query and return a typed reactive view.
    ///
    /// This is the user-facing observation entry point. The view
    /// holds the underlying [`QueryHandle`] and exposes a borrow-only
    /// surface (`rows()`, `pending()`, `version()`). Drop the view to
    /// decrement the subscription's refcount.
    pub fn observe<Row>(&self, query: Query<Row>) -> QueryView<Row>
    where
        Row: Clone + 'static,
    {
        QueryView {
            handle: self.subscribe(query),
        }
    }

    /// Run a mutator end-to-end: apply optimistic locally, push to the
    /// server, reconcile canonical state. The routing engine evaluates
    /// every matching subscription's predicate to decide where the
    /// row changes land — components observing matching queries see
    /// the optimistic instantly, then the canonical replaces it.
    ///
    /// The push wire envelope carries EMPTY params — mutators are
    /// query-agnostic on the wire; routing happens client-side via
    /// predicate evaluation. See the wire/server contract in
    /// `pocopine-sync` and the cookbook.
    pub async fn mutate<M>(
        &self,
        payload: M::Payload,
        ctx: &dyn crate::MutatorRemoteContext,
    ) -> pocopine_sync::SyncResult<crate::MutationOutcome<M::Row>>
    where
        M: crate::Mutator,
        M::Row: Clone + serde::Serialize + 'static,
    {
        // 1. Optimistic apply locally.
        let local_changes = M::apply_local(&payload);
        let stream = pocopine_sync::SyncStreamName::new(M::STREAM)
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
        let mutation_id = ctx.next_mutation_id()?;
        let payload_value = serde_json::to_value(&payload)
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
        let wire_mutation =
            pocopine_sync::ClientMutation::upsert(mutation_id.clone(), payload_value);

        self.route_optimistic_changes::<M::Row>(
            &stream,
            &mutation_id,
            &wire_mutation,
            &local_changes,
        );

        // 2. Wire push (caller-supplied context handles transport).
        let canonical_changes = match M::apply_remote(ctx, payload).await {
            Ok(c) => c,
            Err(err) => {
                // Roll back the optimistic overlay on push failure.
                self.dequeue_pending::<M::Row>(&stream, &mutation_id);
                return Err(err);
            }
        };

        // 3. Reconcile canonical: route every RowChange (upsert AND
        //    delete) into matching subscriptions. Deletes carry a
        //    `RowKey` directly; upserts carry the row payload from
        //    which the routing engine pulls the key via
        //    `row_key_of`.
        self.route_canonical_changes::<M::Row>(&stream, &mutation_id, &canonical_changes);

        Ok(crate::MutationOutcome::Accepted(canonical_changes))
    }

    /// Drop a pending overlay across every matching subscription.
    /// Used to roll back optimistic state when the server push fails.
    /// Fires each affected subscription's `on_update` listeners so
    /// observers re-render the rolled-back state instead of waiting
    /// for an unrelated mutation to refresh them.
    fn dequeue_pending<Row>(&self, stream: &SyncStreamName, mutation_id: &MutationId)
    where
        Row: Clone + 'static,
    {
        let registry = self.inner.registry.borrow();
        for sub in registry.values() {
            if sub.stream() != stream {
                continue;
            }
            let Some(typed): Option<&QuerySubscription<Row>> =
                downcast_ref_subscription(sub.as_any())
            else {
                continue;
            };
            let removed = typed.state.borrow_mut().remove_pending(mutation_id);
            if removed.is_some() {
                typed.notify_listeners();
            }
        }
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
    /// Internal API — consumers use [`QueryClient::mutate`] instead.
    pub(crate) fn route_optimistic_changes<Row>(
        &self,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
        wire_mutation: &ClientMutation<Value>,
        changes: &[RowChange<Row>],
    ) where
        Row: Clone + serde::Serialize + 'static,
    {
        let registry = self.inner.registry.borrow();
        for sub in registry.values() {
            if sub.stream() != stream {
                continue;
            }
            let Some(typed): Option<&QuerySubscription<Row>> =
                downcast_ref_subscription(sub.as_any())
            else {
                continue;
            };
            let mut touched = false;
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
                            touched = true;
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
                        touched = true;
                        let _ = key; // key drives the rebase render layer (TBD)
                    }
                }
            }
            if touched {
                typed.notify_listeners();
            }
        }
    }

    /// Apply canonical row changes returned by a server push.
    /// Removes the corresponding pending overlay and applies each
    /// change to the canonical row set of every matching
    /// subscription:
    ///
    /// * `Upsert(row)` — predicate matches → upsert into canonical;
    ///   predicate doesn't match → remove (the row left this
    ///   subscription's filter set).
    /// * `Delete(key)` — remove from canonical on every matching
    ///   subscription.
    ///
    /// Internal API — consumers use [`QueryClient::mutate`] instead.
    pub(crate) fn route_canonical_changes<Row>(
        &self,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
        canonical: &[RowChange<Row>],
    ) where
        Row: Clone + serde::Serialize + 'static,
    {
        let registry = self.inner.registry.borrow();
        for sub in registry.values() {
            if sub.stream() != stream {
                continue;
            }
            let Some(typed): Option<&QuerySubscription<Row>> =
                downcast_ref_subscription(sub.as_any())
            else {
                continue;
            };
            {
                let mut state = typed.state.borrow_mut();
                // Dequeue the optimistic overlay (idempotent if not present).
                let _ = state.remove_pending(mutation_id);
                for change in canonical {
                    match change {
                        RowChange::Upsert(row) => {
                            let key = match row_key_of(row) {
                                Some(k) => k,
                                None => continue,
                            };
                            if typed.query.matches(row) {
                                state.upsert_canonical(SyncRow {
                                    key,
                                    version: None,
                                    value: row.clone(),
                                    pending: false,
                                    conflict: false,
                                });
                            } else {
                                // Row no longer matches this query's
                                // predicate (e.g. a workspace
                                // transition). Remove from canonical
                                // too.
                                state.remove_canonical(&key);
                            }
                        }
                        RowChange::Delete(key) => {
                            state.remove_canonical(key);
                        }
                    }
                }
            }
            typed.notify_listeners();
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
///
/// Most user code goes through [`QueryView`] which wraps a handle
/// with a read-only API. Use `QueryHandle` directly only when you
/// need the raw `Rc<RefCell<QueryState<Row>>>` for custom reactive
/// integrations.
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

    /// Raw `Rc<RefCell<...>>` over the per-query state. Returned for
    /// callers that want to plumb the state into a custom reactive
    /// system (e.g. integrate with pocopine's `effect()` /
    /// `track()`). Most user code uses [`QueryView`] instead.
    pub fn shared_state(&self) -> Rc<RefCell<QueryState<Row>>> {
        self.subscription.state.clone()
    }
}

/// User-facing read surface over a query subscription.
///
/// Returned from [`QueryClient::observe`]. Holds the underlying
/// [`QueryHandle`] so the subscription is kept alive as long as the
/// view is alive.
///
/// Read APIs (`rows`, `pending`, `version`) borrow from the state's
/// `RefCell`; release the returned borrows before the next mutation
/// goes through the routing engine. Most apps consume the view via
/// the per-tick borrow-and-render pattern (see the cookbook).
pub struct QueryView<Row: 'static> {
    handle: QueryHandle<Row>,
}

impl<Row: 'static> QueryView<Row> {
    /// Underlying query identity.
    pub fn query(&self) -> &Query<Row> {
        self.handle.query()
    }

    /// Borrow the per-query state.
    pub fn state(&self) -> std::cell::Ref<'_, QueryState<Row>> {
        self.handle.state()
    }

    /// Current canonical row count.
    pub fn len(&self) -> usize {
        self.state().canonical_len()
    }

    /// True when the view has no canonical rows AND no pending
    /// overlays.
    pub fn is_empty(&self) -> bool {
        let state = self.state();
        state.canonical_len() == 0 && state.pending().is_empty()
    }

    /// Borrow the canonical rows as a vector copy. Convenient when
    /// callers want to iterate without holding a `Ref<_>` across
    /// state mutations.
    pub fn rows(&self) -> Vec<Row>
    where
        Row: Clone,
    {
        self.state()
            .canonical_rows()
            .map(|r| r.value.clone())
            .collect()
    }

    /// Monotonic state-version counter. Bumps on every mutation
    /// (canonical or optimistic) so a reactive observer can track
    /// changes by reading this single integer.
    pub fn version(&self) -> u64 {
        self.state().version()
    }

    /// Borrow the shared state for custom reactive integration.
    /// Same `Rc<RefCell<...>>` the routing engine writes through.
    pub fn shared_state(&self) -> Rc<RefCell<QueryState<Row>>> {
        self.handle.shared_state()
    }

    /// Register a callback to fire after every state-mutating
    /// operation on this view's subscription.
    ///
    /// The callback runs from inside the routing engine, AFTER the
    /// state is updated but BEFORE the caller's `await` resumes —
    /// any reads of the view inside the callback see the new state.
    ///
    /// Returned token unregisters the callback on drop. Hold it in
    /// the calling scope for the lifetime of the observation.
    ///
    /// Pocopine reactivity integration pattern (from inside a
    /// component handler):
    ///
    /// ```ignore
    /// let view = Issues::query().workspace_id(w1).observe(&qc);
    /// let scope = pocopine_core::current_scope_id().unwrap();
    /// let _token = view.on_update(move || {
    ///     pocopine_core::scope::notify(scope, "issues_view");
    /// });
    /// pocopine_core::effect(move || {
    ///     pocopine_core::track(scope, "issues_view");
    ///     render_view(&view.rows());
    /// });
    /// ```
    pub fn on_update<F>(&self, callback: F) -> UpdateToken<Row>
    where
        F: Fn() + 'static,
    {
        let id = self.handle.subscription.register_listener(callback);
        UpdateToken {
            subscription: self.handle.subscription.clone(),
            id,
        }
    }
}

/// Token returned from [`QueryView::on_update`]. Drop to unregister
/// the callback.
pub struct UpdateToken<Row: 'static> {
    subscription: Rc<QuerySubscription<Row>>,
    id: u64,
}

impl<Row: 'static> Drop for UpdateToken<Row> {
    fn drop(&mut self) {
        self.subscription.unregister_listener(self.id);
    }
}

impl<Row: 'static> Drop for QueryHandle<Row> {
    fn drop(&mut self) {
        self.registry.release();
    }
}

/// Back-reference from a [`QueryHandle`] to its [`QueryClient`].
/// Holds a `Weak<QueryClientInner>` so a handle that outlives the
/// client (e.g. handle returned from a nested block where the
/// client itself was dropped first) sees `Weak::upgrade → None`
/// and no-ops the refcount decrement. Safe Rust — no raw pointers.
struct RegistryWeakRef {
    client: Weak<QueryClientInner>,
    key: QueryKey,
}

impl RegistryWeakRef {
    fn new(client: &Rc<QueryClientInner>, key: QueryKey) -> Self {
        Self {
            client: Rc::downgrade(client),
            key,
        }
    }

    fn release(&self) {
        if let Some(inner) = self.client.upgrade() {
            release_inner(&inner, self.key);
        }
    }
}

fn release_inner(inner: &QueryClientInner, key: QueryKey) {
    let mut registry = inner.registry.borrow_mut();
    let should_remove = registry
        .get(&key)
        .map(|sub| sub.decrement_refcount() == 0)
        .unwrap_or(false);
    if should_remove {
        registry.remove(&key);
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
