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
use crate::query::{MatchFn, Order, Query, QueryKey};
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

        // Arm a cancellation guard. If the mutate future is dropped
        // BEFORE we explicitly disarm it (success or handled err
        // path), the guard's Drop rolls back the optimistic overlay
        // — without this, futures dropped via `tokio::select!`,
        // task abort, or any other early-cancel path leave the
        // optimistic overlay alive in the subscription's pending
        // queue forever, rendering a row the server never received.
        let guard: RollbackGuard<'_, M::Row> = RollbackGuard {
            client: Some(self),
            stream: stream.clone(),
            mutation_id: mutation_id.clone(),
            _row: std::marker::PhantomData,
        };

        // 2. Wire push (caller-supplied context handles transport).
        let canonical_changes = match M::apply_remote(ctx, payload).await {
            Ok(c) => c,
            Err(err) => {
                // Roll back the optimistic overlay on push failure
                // via the guard's Drop — disarm not called, so when
                // `guard` falls out of scope at function return its
                // Drop runs `dequeue_pending` exactly once.
                drop(guard);
                return Err(err);
            }
        };

        // 3. Reconcile canonical: route every RowChange (upsert AND
        //    delete) into matching subscriptions. Deletes carry a
        //    `RowKey` directly; upserts carry the row payload from
        //    which the routing engine pulls the key via
        //    `row_key_of`.
        self.route_canonical_changes::<M::Row>(&stream, &mutation_id, &canonical_changes);

        // Success path: disarm the guard so its Drop is a no-op
        // (the canonical reconcile is authoritative; the optimistic
        // overlay was already dequeued by `route_canonical_changes`
        // via `state.remove_pending`).
        guard.disarm();

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
            let removed = {
                let mut state = typed.state.borrow_mut();
                let overlays = state.remove_pending(mutation_id);
                // Restore any canonical rows that were optimistically
                // removed by Delete or predicate-departure overlays —
                // each overlay carries the displaced row in
                // `deleted_row`. Without this, a rejected delete (or
                // a rejected status-change that knocked a row out of
                // its filter) would leave the view empty until a
                // server-side refresh.
                //
                // Conservative restore: only re-upsert when the key is
                // NOT currently in canonical. If a concurrent
                // successful mutation (or a server push) landed a
                // newer canonical row for the same key while this
                // mutation was in flight, the stale snapshot would
                // clobber it. Skipping the restore in that case lets
                // the newer canonical state stand; eventual
                // consistency via the next `/pull` handles any
                // genuine divergence.
                for overlay in &overlays {
                    if let Some(restored) = overlay.deleted_row.clone() {
                        if !state.canonical_contains(&restored.key) {
                            state.upsert_canonical(restored);
                        }
                    }
                }
                overlays
            };
            if !removed.is_empty() {
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
                        // Drop rows without an extractable key on the
                        // floor — there's no way to render them and
                        // pushing an all-None overlay would just leak
                        // pending state. Mirrors the predicate-
                        // departure branch's row_key_of gate.
                        let Some(key) = row_key_of(row) else {
                            continue;
                        };
                        let matches = typed.matches(row);
                        if matches {
                            let mut state = typed.state.borrow_mut();
                            let overlay = PendingOverlay {
                                mutation_id: mutation_id.clone(),
                                mutation: wire_mutation.clone(),
                                optimistic_row: Some(SyncRow {
                                    key,
                                    version: None,
                                    value: row.clone(),
                                    pending: true,
                                    conflict: false,
                                }),
                                deleted_row: None,
                                evicted_key: None,
                                conflict: false,
                            };
                            state.push_pending(overlay);
                            touched = true;
                        } else {
                            // Optimistic predicate-DEPARTURE: a row
                            // that previously satisfied this query
                            // but no longer does (e.g. an issue
                            // transitioning from `Open` to `Closed`
                            // in a query filtered to `Open`-only)
                            // must be optimistically removed from
                            // the view. Visibility includes BOTH
                            // canonical AND any prior pending
                            // optimistic_row for the same key — a
                            // row that's only-pending still needs to
                            // get evicted from the rendered view.
                            let (was_visible, canonical_snapshot) = {
                                let state = typed.state.borrow();
                                let canonical = state.canonical_get(&key).cloned();
                                let in_pending = state.pending().iter().any(|p| {
                                    p.optimistic_row
                                        .as_ref()
                                        .map(|r| r.key == key)
                                        .unwrap_or(false)
                                });
                                (canonical.is_some() || in_pending, canonical)
                            };
                            if was_visible {
                                let mut state = typed.state.borrow_mut();
                                state.push_pending(PendingOverlay::<Row> {
                                    mutation_id: mutation_id.clone(),
                                    mutation: wire_mutation.clone(),
                                    optimistic_row: None,
                                    deleted_row: canonical_snapshot,
                                    evicted_key: Some(key.clone()),
                                    conflict: false,
                                });
                                state.remove_canonical(&key);
                                touched = true;
                            }
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
                        let (row_visible, canonical_snapshot) = {
                            let state = typed.state.borrow();
                            let canonical = state.canonical_get(key).cloned();
                            let in_pending = state.pending().iter().any(|p| {
                                p.optimistic_row
                                    .as_ref()
                                    .map(|r| &r.key == key)
                                    .unwrap_or(false)
                            });
                            (canonical.is_some() || in_pending, canonical)
                        };
                        if row_visible {
                            let mut state = typed.state.borrow_mut();
                            state.push_pending(PendingOverlay::<Row> {
                                mutation_id: mutation_id.clone(),
                                mutation: wire_mutation.clone(),
                                optimistic_row: None,
                                // Capture the canonical row (if any)
                                // so rollback can restore it. A
                                // delete that targets only an
                                // optimistic upsert leaves
                                // `deleted_row` None — there's
                                // nothing canonical to restore.
                                deleted_row: canonical_snapshot,
                                evicted_key: Some(key.clone()),
                                conflict: false,
                            });
                            // Optimistic delete: ALSO remove the row
                            // from canonical_rows so `view.rows()`
                            // reflects the disappearance immediately.
                            // The canonical reconcile path will
                            // re-confirm the absence (or rollback via
                            // `deleted_row` snapshot if push fails).
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
                // Dequeue the optimistic overlays (idempotent). Note
                // we DON'T restore `deleted_row` here — the server
                // accepted the mutation, so the canonical reconcile
                // below is authoritative; the snapshot is only used
                // on rollback (see `dequeue_pending`).
                if !state.remove_pending(mutation_id).is_empty() {
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

/// Cancellation-safety guard for `QueryClient::mutate`. The
/// optimistic apply happens synchronously before any `.await`; if
/// the mutate future is dropped between the optimistic apply and
/// the canonical reconcile (a cancelled `tokio::select!` branch, a
/// task abort, a `Drop` of the awaiting task), this guard's `Drop`
/// runs `dequeue_pending` exactly once — rolling back the
/// optimistic overlay. The success and handled-error paths call
/// `disarm` to make the `Drop` a no-op (`dequeue_pending` for the
/// success path has already happened via `route_canonical_changes`;
/// the error path runs it manually via `drop(guard)`).
struct RollbackGuard<'a, Row: Clone + 'static> {
    /// `None` once `disarm()` has been called — Drop becomes a no-op.
    client: Option<&'a QueryClient>,
    stream: SyncStreamName,
    mutation_id: MutationId,
    _row: std::marker::PhantomData<fn() -> Row>,
}

impl<Row: Clone + 'static> RollbackGuard<'_, Row> {
    fn disarm(mut self) {
        self.client = None;
    }
}

impl<Row: Clone + 'static> Drop for RollbackGuard<'_, Row> {
    fn drop(&mut self) {
        if let Some(client) = self.client {
            client.dequeue_pending::<Row>(&self.stream, &self.mutation_id);
        }
    }
}

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

/// Compare two `serde_json::Value`s for the rendered-rows
/// `order_by` sort. The implementation MUST be a total order
/// (reflexive, antisymmetric, transitive) — `sort_by` is allowed
/// to panic on stdlib total-order debug assertions otherwise.
///
/// `None` and `Some(Value::Null)` are treated identically as
/// "absent"; absent sorts as `Less` than every present value
/// (asc → absents first, desc → absents last). Numbers compare
/// losslessly via `as_i64`/`as_u64` before falling back to `f64`
/// (so distinct `u64` values above `2^53` don't collide).
/// Strings compare lexicographically, bools (false < true).
/// Arrays/objects/cross-type fall back to `Equal`; the sort in
/// `QueryView::rows` is stable, so equal keys keep their
/// `BTreeMap` (RowKey-ordered) insertion order.
fn json_value_cmp(a: Option<&Value>, b: Option<&Value>) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    fn is_absent(v: Option<&Value>) -> bool {
        matches!(v, None | Some(Value::Null))
    }

    match (is_absent(a), is_absent(b)) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => {
            // Both Some(non-Null); the inner unwraps are safe.
            let a = a.expect("checked by is_absent");
            let b = b.expect("checked by is_absent");
            match (a, b) {
                (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
                (Value::Number(x), Value::Number(y)) => json_number_cmp(x, y),
                (Value::String(x), Value::String(y)) => x.cmp(y),
                _ => Ordering::Equal,
            }
        }
    }
}

/// Compare two `serde_json::Number` values. Tries lossless
/// integer compare first (both fit in `i64`, or both fit in `u64`),
/// then falls back to `f64` for floats or mixed-sign large
/// integers. Avoids the `u64 → f64` precision-loss collision that
/// makes `u64::MAX` and `u64::MAX - 100` compare equal.
fn json_number_cmp(x: &serde_json::Number, y: &serde_json::Number) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let (Some(xi), Some(yi)) = (x.as_i64(), y.as_i64()) {
        return xi.cmp(&yi);
    }
    if let (Some(xu), Some(yu)) = (x.as_u64(), y.as_u64()) {
        return xu.cmp(&yu);
    }
    // Mixed-sign large integer OR contains a float: fall back to f64.
    x.as_f64()
        .partial_cmp(&y.as_f64())
        .unwrap_or(Ordering::Equal)
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
    /// applied and optimistic deletes/predicate-departures
    /// suppressed. Short-circuits to a cheap `rendered_key_count()`
    /// walk when the query has no `order_by` and no `limit`;
    /// otherwise materializes via `rows()` (sort + truncate).
    pub fn len(&self) -> usize
    where
        Row: Clone + serde::Serialize,
    {
        let query = self.query();
        if query.order_by().is_none() && query.limit().is_none() {
            return self.rendered_key_count();
        }
        self.rows().len()
    }

    /// True when the rendered row set is empty AND there are no
    /// pending overlays.
    pub fn is_empty(&self) -> bool
    where
        Row: Clone + serde::Serialize,
    {
        // Cheap path: don't materialize rows just to ask "anything
        // visible?". A `rendered_key_count` walk visits each
        // canonical row and overlay once; no row cloning or JSON
        // serialization. The pending-empty check matches the
        // documented "idle view" semantics.
        self.rendered_key_count() == 0 && self.state().pending().is_empty()
    }

    /// Number of distinct keys that survive the canonical + pending
    /// merge (insertions from `optimistic_row`, evictions from
    /// `evicted_key`). Used by `len()` / `is_empty()` to count
    /// without cloning every row or JSON-serializing for sort.
    fn rendered_key_count(&self) -> usize {
        use std::collections::BTreeSet;
        let state = self.state();
        let mut keys: BTreeSet<&pocopine_sync::RowKey> =
            state.canonical_rows().map(|r| &r.key).collect();
        for overlay in state.pending() {
            if let Some(opt_row) = &overlay.optimistic_row {
                keys.insert(&opt_row.key);
            } else if let Some(evicted) = &overlay.evicted_key {
                keys.remove(evicted);
            }
        }
        keys.len()
    }

    /// The view's current rendered row set: canonical rows MERGED
    /// with optimistic pending overlays, ORDERED per the query's
    /// `order_by()` and TRUNCATED per its `limit()`.
    ///
    /// Without an `order_by`, rows are returned in `RowKey` order
    /// (the BTreeMap's natural ordering). The merge applies pending
    /// overlays in apply order — an `optimistic_row` inserts /
    /// overwrites, an `evicted_key` removes. So a later Delete or
    /// predicate-departure correctly hides a key whose only
    /// visibility came from an earlier Upsert overlay (not yet in
    /// canonical).
    ///
    /// Ordering reads the field via serde's JSON projection: a row
    /// type's serialization MUST expose the order-by field as a top-
    /// level key for the comparator to find it. Rows that don't
    /// expose the field sort as "less than" any present value
    /// (stable sort; original BTreeMap order tie-breaks).
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
            } else if let Some(evicted) = &overlay.evicted_key {
                rendered.remove(evicted);
            }
        }
        let mut rows: Vec<Row> = rendered.into_values().collect();

        let query = self.query();
        if let Some(order_by) = query.order_by() {
            let field = order_by.field.as_str();
            let direction = order_by.direction;
            rows.sort_by(|a, b| {
                let a_key = serde_json::to_value(a)
                    .ok()
                    .and_then(|v| v.get(field).cloned());
                let b_key = serde_json::to_value(b)
                    .ok()
                    .and_then(|v| v.get(field).cloned());
                let cmp = json_value_cmp(a_key.as_ref(), b_key.as_ref());
                match direction {
                    Order::Asc => cmp,
                    Order::Desc => cmp.reverse(),
                }
            });
        }

        if let Some(limit) = query.limit() {
            rows.truncate(limit as usize);
        }

        rows
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

    /// Regression for the json_value_cmp total-order violation.
    /// `(Some(Null), Some(Null))` previously returned `Less` (via
    /// the `(Some(Null), _)` arm), which violates antisymmetry —
    /// `cmp(a, b)` and `cmp(b, a)` must agree on Equal. Now both
    /// directions return Equal, and an absent value sorts as Less
    /// vs a present one consistently.
    #[test]
    fn json_value_cmp_null_is_total_order() {
        use std::cmp::Ordering;
        let null = Value::Null;
        let s = Value::String("a".into());

        assert_eq!(json_value_cmp(Some(&null), Some(&null)), Ordering::Equal);
        assert_eq!(json_value_cmp(None, None), Ordering::Equal);
        assert_eq!(json_value_cmp(None, Some(&null)), Ordering::Equal);
        assert_eq!(json_value_cmp(Some(&null), None), Ordering::Equal);
        assert_eq!(json_value_cmp(Some(&null), Some(&s)), Ordering::Less);
        assert_eq!(json_value_cmp(Some(&s), Some(&null)), Ordering::Greater);
        assert_eq!(json_value_cmp(None, Some(&s)), Ordering::Less);
        assert_eq!(json_value_cmp(Some(&s), None), Ordering::Greater);
    }

    /// Regression for the f64 precision-loss bug in
    /// `json_value_cmp`. Two distinct u64 values above 2^53 used
    /// to round to the same f64 → `partial_cmp` returned Equal →
    /// stable sort silently collapsed them. The new
    /// `json_number_cmp` tries `as_u64` first, comparing losslessly.
    #[test]
    fn json_value_cmp_handles_u64_above_pow_2_53() {
        use std::cmp::Ordering;
        let small = Value::Number(serde_json::Number::from(u64::MAX - 100));
        let big = Value::Number(serde_json::Number::from(u64::MAX));
        // Sanity check the precondition: both round to the same f64.
        assert_eq!(small.as_f64(), big.as_f64(), "precondition: f64 collision");
        // But our comparator must distinguish them.
        assert_eq!(json_value_cmp(Some(&small), Some(&big)), Ordering::Less);
        assert_eq!(json_value_cmp(Some(&big), Some(&small)), Ordering::Greater);
    }
}
