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

use std::any::{Any, TypeId};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use pocopine_sync::{ClientMutation, MutationId, SyncRow, SyncStreamName, SYNC_ENDPOINT_PREFIX};
use serde_json::Value;

use crate::mutator::RowChange;
use crate::query::{MatchFn, Query, QueryKey};
use crate::state::{PendingOverlay, QueryState};

/// Composite registry key. Including the `TypeId` of the row type
/// prevents two queries with the same `(stream, params, order, limit)`
/// but different `Row` types from colliding — a collision used to
/// trigger `downcast_subscription.expect(...)` panics.
type RegistryKey = (TypeId, QueryKey);

/// Untyped subscription trait — needed so the registry can hold
/// subscriptions of any `Row` type behind one map.
///
/// Each impl is for `QuerySubscription<Row>` where `Row: 'static`,
/// so the type carries an `Any` upcast for safe downcasting via
/// `Rc::downcast` (replacing the prior `Rc::into_raw`/`from_raw`
/// hack that relied on `RcBox<dyn>` / `RcBox<Concrete>` layout
/// coincidence).
trait AnyQuerySubscription: 'static {
    fn stream(&self) -> &SyncStreamName;
    fn refcount(&self) -> usize;
    fn bump_refcount(&self) -> usize;
    fn decrement_refcount(&self) -> usize;
    /// Re-package self into `Rc<dyn Any>` for safe `Rc::downcast`.
    /// The default-method-style cannot work here because `Rc::downcast`
    /// requires the source to be `Rc<dyn Any>`, not `Rc<dyn OtherTrait>`.
    fn as_rc_any(self: Rc<Self>) -> Rc<dyn Any>;
}

/// One live subscription against `(stream, params)`.
///
/// Owns its own state, refcount, and (in subsequent PRs) live-wakeup
/// and background-task lifecycle. The state is `Rc<RefCell<...>>` so
/// the routing engine and any reactive views can hold concurrent
/// borrows in a single-threaded runtime.
/// Update listener handle. Stored as `Rc<dyn Fn()>` (not `Box`) so
/// `notify_listeners` can snapshot the handles into a local vec
/// before invoking — letting callbacks safely drop their
/// `UpdateToken` or register a new listener without re-entering the
/// `listeners` RefCell.
type ListenerFn = Rc<dyn Fn()>;

/// `(id, callback)` entry in a subscription's listener list. The id
/// is what [`UpdateToken`] keeps so drop can unregister.
type ListenerEntry = (u64, ListenerFn);

pub struct QuerySubscription<Row: 'static> {
    pub(crate) query: Query<Row>,
    /// Best-known predicate evaluator. Subscribed queries may upgrade
    /// this from `None` to `Some(fn)` — e.g., a hand-built Query<Row>
    /// (no macro) subscribes first, then a macro-built one with the
    /// same `(TypeId, QueryKey)` subscribes second and provides the
    /// real predicate. Storing the latest non-None value here avoids
    /// silent stale-predicate routing.
    pub(crate) matches_fn: Cell<Option<MatchFn<Row>>>,
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
        let matches_fn = Cell::new(query.matches_fn);
        Self {
            query,
            matches_fn,
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

    /// Evaluate this subscription's predicate against `row`. Uses the
    /// most-recently-installed [`MatchFn`]; defaults to `true` when
    /// no predicate has been set (a hand-built `Query<Row>` with no
    /// macro). Routing engine uses this — NOT [`Query::matches`] —
    /// so a hand-built subscription that's later joined by a
    /// macro-built one starts honoring the macro's predicate.
    pub(crate) fn matches(&self, row: &Row) -> bool {
        match self.matches_fn.get() {
            Some(f) => f(&self.query, row),
            None => true,
        }
    }

    /// Upgrade the predicate evaluator if the new one is `Some` and
    /// the existing one is `None`. Never downgrades. Called by
    /// `QueryClient::subscribe` when an existing entry is reused.
    pub(crate) fn maybe_upgrade_matches_fn(&self, candidate: Option<MatchFn<Row>>) {
        if candidate.is_some() && self.matches_fn.get().is_none() {
            self.matches_fn.set(candidate);
        }
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
        self.listeners.borrow_mut().push((id, Rc::new(callback)));
        id
    }

    /// Drop a previously-registered listener by id. Uses `try_borrow_mut`
    /// so an `UpdateToken::drop` that runs while another panic is
    /// unwinding silently no-ops instead of triggering a double-panic
    /// abort. The matching listener stays alive in the list until the
    /// next non-borrowed unregister call.
    pub(crate) fn unregister_listener(&self, id: u64) {
        if let Ok(mut listeners) = self.listeners.try_borrow_mut() {
            listeners.retain(|(lid, _)| *lid != id);
        }
    }

    /// Fire every registered listener.
    ///
    /// Snapshots the listener list into a local `Vec<Rc<dyn Fn()>>`
    /// BEFORE invoking any callback, then releases the `listeners`
    /// borrow. This makes the following user-code patterns safe:
    ///
    /// * A callback that drops an `UpdateToken` (which `borrow_mut`s
    ///   `listeners`) — runs without re-entry panic.
    /// * A callback that registers a new listener via
    ///   `view.on_update(...)` — the new listener is added to the
    ///   underlying `Vec`, but isn't invoked in this notify pass.
    fn notify_listeners(&self) {
        // Take a snapshot of the listener Rc handles. Listener
        // identities are stable across this snapshot — they refer to
        // the same `Rc<dyn Fn()>` even if the underlying Vec is
        // mutated concurrently by a callback below.
        let snapshot: Vec<Rc<dyn Fn()>> = {
            let listeners = self.listeners.borrow();
            listeners.iter().map(|(_, cb)| cb.clone()).collect()
        };
        for cb in snapshot {
            cb();
        }
    }
}

impl<Row: 'static> AnyQuerySubscription for QuerySubscription<Row> {
    fn stream(&self) -> &SyncStreamName {
        self.query.stream()
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

    fn as_rc_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
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
    /// Active subscriptions keyed by `(Row TypeId, QueryKey)`. Two
    /// queries with the same `(stream, params, order, limit)` but
    /// different `Row` types live in separate entries — no
    /// `Rc<dyn Any>` downcast can panic on a Row mismatch.
    registry: RefCell<HashMap<RegistryKey, Rc<dyn AnyQuerySubscription>>>,
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
    /// over the same underlying `QuerySubscription`. When the new
    /// query carries a `matches_fn` and the existing subscription's
    /// is `None`, the predicate is upgraded so a hand-built Query
    /// (no macro) followed by a macro-built one starts honoring
    /// the macro's predicate.
    pub fn subscribe<Row>(&self, query: Query<Row>) -> QueryHandle<Row>
    where
        Row: 'static,
    {
        let registry_key: RegistryKey = (TypeId::of::<Row>(), query.key());
        let candidate_matches_fn = query.matches_fn;
        let mut registry = self.inner.registry.borrow_mut();
        let entry = registry.entry(registry_key).or_insert_with(|| {
            Rc::new(QuerySubscription::new(query)) as Rc<dyn AnyQuerySubscription>
        });
        entry.bump_refcount();
        // Safe downcast via `Rc<dyn Any>`. The `(TypeId, QueryKey)`
        // composite key guarantees the entry's concrete `Row` type
        // matches the caller's, so this downcast is total — no
        // possibility of panic on `Row` collision.
        let typed: Rc<QuerySubscription<Row>> = Rc::downcast::<QuerySubscription<Row>>(
            entry.clone().as_rc_any(),
        )
        .expect(
            "(TypeId, QueryKey) collision keys to wrong Row type — framework invariant violated",
        );
        // Upgrade the stored predicate if the caller has a Some()
        // matches_fn and the existing one is None.
        typed.maybe_upgrade_matches_fn(candidate_matches_fn);
        QueryHandle {
            subscription: typed,
            registry: RegistryWeakRef::new(&self.inner, registry_key),
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
        let key: RegistryKey = (TypeId::of::<Row>(), query.key());
        self.inner
            .registry
            .borrow()
            .get(&key)
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
        // Derive the wire `SyncOp` from the mutator's local row
        // changes. A mutator that only produces deletes is encoded
        // as a Delete on the wire; a mutator that produces both
        // (rare) defaults to Upsert because the per-mutation
        // envelope can only carry one op. For pure-delete mutators
        // (the common case for "remove" actions), this avoids
        // sending op=Upsert with a key-shaped payload which would
        // corrupt the server's mutation log.
        let wire_op = if !local_changes.is_empty()
            && local_changes
                .iter()
                .all(|c| matches!(c, RowChange::Delete(_)))
        {
            pocopine_sync::SyncOp::Delete
        } else {
            pocopine_sync::SyncOp::Upsert
        };
        let wire_mutation =
            pocopine_sync::ClientMutation::new(mutation_id.clone(), wire_op, payload_value);

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
        for typed in self.collect_subscriptions_on_stream::<Row>(stream) {
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
    ///
    /// Snapshots matching subscriptions into a local Vec BEFORE
    /// mutating state / firing listeners, so callbacks can safely
    /// call back into the client (e.g. `client.observe(...)`,
    /// `client.mutate(...)`, or drop an `UpdateToken`) without
    /// re-entering the `registry` RefCell.
    pub(crate) fn route_optimistic_changes<Row>(
        &self,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
        wire_mutation: &ClientMutation<Value>,
        changes: &[RowChange<Row>],
    ) where
        Row: Clone + serde::Serialize + 'static,
    {
        for typed in self.collect_subscriptions_on_stream::<Row>(stream) {
            let mut touched = false;
            for change in changes {
                match change {
                    RowChange::Upsert(row) => {
                        if typed.matches(row) {
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
                        // Predicate-gate the delete: a subscription
                        // only gets a Delete overlay when the row
                        // identified by `key` is currently visible
                        // (in canonical or as an optimistic upsert).
                        // Without this gate, every delete would
                        // broadcast a no-op overlay to every
                        // subscription on the stream, spuriously
                        // marking unrelated views as non-empty and
                        // firing their listeners.
                        let row_visible = {
                            let state = typed.state.borrow();
                            state.canonical_rows().any(|r| &r.key == key)
                                || state.pending().iter().any(|p| {
                                    p.optimistic_row
                                        .as_ref()
                                        .map(|r| &r.key == key)
                                        .unwrap_or(false)
                                })
                        };
                        if row_visible {
                            let mut state = typed.state.borrow_mut();
                            state.push_pending(PendingOverlay::<Row> {
                                mutation_id: mutation_id.clone(),
                                mutation: wire_mutation.clone(),
                                optimistic_row: None,
                                conflict: false,
                            });
                            // Optimistic delete: ALSO remove the row
                            // from canonical_rows so `view.rows()`
                            // reflects the disappearance immediately.
                            // The canonical reconcile path will
                            // re-confirm the absence (or restore via
                            // rollback if the push fails).
                            state.remove_canonical(key);
                            touched = true;
                        }
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
    /// Fires `notify_listeners` only when the subscription's state
    /// actually changed — avoids over-notifying observers of
    /// unrelated subscriptions on cross-tenant mutations.
    pub(crate) fn route_canonical_changes<Row>(
        &self,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
        canonical: &[RowChange<Row>],
    ) where
        Row: Clone + serde::Serialize + 'static,
    {
        for typed in self.collect_subscriptions_on_stream::<Row>(stream) {
            let touched = {
                let mut state = typed.state.borrow_mut();
                let mut touched = false;
                // Dequeue the optimistic overlay (idempotent).
                if state.remove_pending(mutation_id).is_some() {
                    touched = true;
                }
                for change in canonical {
                    match change {
                        RowChange::Upsert(row) => {
                            let key = match row_key_of(row) {
                                Some(k) => k,
                                None => continue,
                            };
                            if typed.matches(row) {
                                state.upsert_canonical(SyncRow {
                                    key,
                                    version: None,
                                    value: row.clone(),
                                    pending: false,
                                    conflict: false,
                                });
                                touched = true;
                            } else if state.canonical_rows().any(|r| r.key == key) {
                                state.remove_canonical(&key);
                                touched = true;
                            }
                        }
                        RowChange::Delete(key) => {
                            if state.canonical_rows().any(|r| &r.key == key) {
                                state.remove_canonical(key);
                                touched = true;
                            }
                        }
                    }
                }
                touched
            };
            if touched {
                typed.notify_listeners();
            }
        }
    }

    /// Snapshot all subscriptions on a given stream whose Row type
    /// matches `Row`. The returned Vec holds `Rc` clones so callers
    /// can freely call back into the `QueryClient` from inside the
    /// iteration body — no `registry` borrow is held across the
    /// `notify_listeners` invocations downstream.
    fn collect_subscriptions_on_stream<Row: 'static>(
        &self,
        stream: &SyncStreamName,
    ) -> Vec<Rc<QuerySubscription<Row>>> {
        let target = (TypeId::of::<Row>(), ());
        let registry = self.inner.registry.borrow();
        registry
            .iter()
            .filter(|((tid, _), sub)| *tid == target.0 && sub.stream() == stream)
            .filter_map(|(_, sub)| {
                Rc::downcast::<QuerySubscription<Row>>(sub.clone().as_rc_any()).ok()
            })
            .collect()
    }
}

impl Default for QueryClient {
    fn default() -> Self {
        Self::new()
    }
}

// (Routing engine downcasts via `collect_subscriptions_on_stream`,
// which uses `Rc::downcast::<QuerySubscription<Row>>` via the
// `as_rc_any` upcast — fully safe stdlib path.)

/// Best-effort row-key extractor.
///
/// Requires the row's serialization to expose an `"id"` field of
/// JSON String type. Non-string ids (numbers, bool, null, arrays)
/// previously fell through to `Value::to_string()` which JSON-encoded
/// them — two distinct rows could collapse to the same `RowKey` (`42`
/// and `"42"`; multiple `null`s; etc.). Returning `None` for
/// non-string ids surfaces the misuse as a "row silently dropped from
/// routing" symptom — source authors should ensure their row types
/// serialize `id` as a string (`id: String`, `id: SomeWrapper`
/// where the wrapper's serde repr is a string, etc.).
///
/// A future trait `HasRowKey` will replace this serialization-based
/// extractor with a typed contract; until then, the JSON-string
/// constraint is the wire-contract proxy.
fn row_key_of<Row>(row: &Row) -> Option<pocopine_sync::RowKey>
where
    Row: serde::Serialize,
{
    let value = serde_json::to_value(row).ok()?;
    let id = value.get("id")?;
    let id_str = match id {
        Value::String(s) => s.clone(),
        // Non-string ids: refuse rather than JSON-stringify. See
        // doc comment above for the collision rationale.
        _ => return None,
    };
    pocopine_sync::RowKey::new(id_str).ok()
}

// (Owning downcast lives inline at the `subscribe` call site, which
// uses `Rc::downcast::<QuerySubscription<Row>>(rc.as_rc_any())` — a
// fully-safe path supported by the stdlib that replaces the prior
// `Rc::into_raw`/`from_raw` cast that relied on `RcBox<dyn T>` /
// `RcBox<T>` layout coincidence.)

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

    /// Number of rendered rows: canonical, with optimistic upserts
    /// applied. (Optimistic deletes are reflected by the routing
    /// engine removing the row from `canonical_rows` immediately, so
    /// they don't need to be counted out here.)
    pub fn len(&self) -> usize
    where
        Row: Clone + serde::Serialize,
    {
        self.rows().len()
    }

    /// True when the rendered row set is empty AND there are no
    /// pending overlays.
    pub fn is_empty(&self) -> bool
    where
        Row: Clone + serde::Serialize,
    {
        self.rows().is_empty() && self.state().pending().is_empty()
    }

    /// The view's current rendered row set: canonical rows MERGED
    /// with optimistic pending overlays. Rows are returned in
    /// `RowKey` order; if both a canonical and an optimistic row
    /// share a key, the optimistic wins (that's the point — local
    /// edits are visible immediately).
    pub fn rows(&self) -> Vec<Row>
    where
        Row: Clone + serde::Serialize,
    {
        use std::collections::BTreeMap;
        let state = self.state();
        let mut rendered: BTreeMap<pocopine_sync::RowKey, Row> = state
            .canonical_rows()
            .map(|r| (r.key.clone(), r.value.clone()))
            .collect();
        for overlay in state.pending() {
            if let Some(opt_row) = &overlay.optimistic_row {
                rendered.insert(opt_row.key.clone(), opt_row.value.clone());
            }
        }
        rendered.into_values().collect()
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
    /// let view = Issues::query().eq(field::workspace_id, w1).observe(&qc);
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
    key: RegistryKey,
}

impl RegistryWeakRef {
    fn new(client: &Rc<QueryClientInner>, key: RegistryKey) -> Self {
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

fn release_inner(inner: &QueryClientInner, key: RegistryKey) {
    // `try_borrow_mut` so a Drop running while the routing engine
    // already holds the registry borrow no-ops instead of
    // double-panicking. The decrement will happen when the next
    // entry-touching op runs — refcount staleness is bounded.
    let Ok(mut registry) = inner.registry.try_borrow_mut() else {
        return;
    };
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
