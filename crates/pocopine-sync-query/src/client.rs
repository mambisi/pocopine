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
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use pocopine_sync::{
    ClientMutation, MutationId, RowKey, SyncRow, SyncStreamName, SYNC_ENDPOINT_PREFIX,
};
use serde_json::Value;

use crate::driver::{
    spawn_driver, DriverEpoch, DriverHandle, QueryClientConfig, ReplayEntry, ReplayOutcome,
    SubscriptionDriver,
};
use crate::mutator::{
    wrap_persisted_payload, AnyMutator, HydrateReplayOutcome, MutatorEntry, RowChange,
};
use crate::query::{MatchFn, Order, Query, QueryKey};
use crate::selector::{AnyArgs, AnySelector, SelectorEntry, SelectorId, SelectorView};
use crate::state::{PendingOverlay, QueryState};

/// Composite registry key. Including the `TypeId` of the row type
/// prevents two queries with the same `(stream, params, order, limit)`
/// but different `Row` types from colliding — a collision used to
/// trigger `downcast_subscription.expect(...)` panics.
type RegistryKey = (TypeId, QueryKey);

/// One bucket in the selector registry. Each bucket holds the live
/// `Weak`s for entries whose args happen to hash to the same
/// `(SelectorId, args_hash)` slot — the bucket lookup disambiguates
/// by `AnyArgs::dyn_eq` per [`QueryClient::observe_selector`].
type SelectorBucket = Vec<Weak<dyn AnySelector>>;

/// Untyped subscription trait — needed so the registry can hold
/// subscriptions of any `Row` type behind one map.
///
/// Each impl is for `QuerySubscription<Row>` where `Row: 'static`,
/// so the type carries an `Any` upcast for safe downcasting via
/// `Rc::downcast` (replacing the prior `Rc::into_raw`/`from_raw`
/// hack that relied on `RcBox<dyn>` / `RcBox<Concrete>` layout
/// coincidence).
pub(crate) trait AnyQuerySubscription: 'static {
    fn stream(&self) -> &SyncStreamName;
    fn refcount(&self) -> usize;
    fn bump_refcount(&self) -> usize;
    fn decrement_refcount(&self) -> usize;
    /// Snapshot of this subscription's driver epoch — bumped on
    /// last-handle drop so the spawned driver exits at its next
    /// `.await`.
    fn driver_epoch(&self) -> DriverEpoch;
    /// `true` once a driver task has been spawned for this
    /// subscription. The flag is sticky for the lifetime of the
    /// subscription; `observe` checks it to avoid double-spawn
    /// when a second `subscribe` lands on the same entry.
    fn driver_spawned(&self) -> bool;
    /// Mark the driver as spawned. The framework calls this once
    /// from `observe` immediately after `spawn_driver` returns.
    fn set_driver_spawned(&self);
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
    /// Generation counter that the spawned driver task watches.
    /// Bumped from `release_inner` on the last-handle drop so the
    /// driver exits at its next `.await` per RFC 087 §8.
    epoch: DriverEpoch,
    /// Whether a driver task has been spawned for this subscription.
    /// Set once by `observe`; never cleared (a re-subscribe after
    /// the entry is GC'd builds a new `QuerySubscription` and the
    /// flag starts fresh).
    driver_spawned: Cell<bool>,
    /// Owned driver handle. Currently a marker (cancellation is
    /// epoch-driven, not abort-driven) — present so the type-shape
    /// can carry per-platform metadata in follow-ups.
    _driver_handle: Cell<Option<DriverHandle>>,
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
            epoch: DriverEpoch::new(),
            driver_spawned: Cell::new(false),
            _driver_handle: Cell::new(None),
        }
    }

    /// Fire `notify_listeners` from a caller outside this module —
    /// the driver path needs to notify after applying a `/open` or
    /// `/pull` response but doesn't have access to the private
    /// `notify_listeners` symbol.
    pub(crate) fn notify_listeners_external(&self) {
        self.notify_listeners();
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
        self.register_listener_rc(Rc::new(callback))
    }

    /// Variant of `register_listener` that takes a pre-built
    /// `Rc<dyn Fn()>`. Used by the selector layer's
    /// `AnyTrackable` impl to avoid re-wrapping a closure that's
    /// already shared (a selector's rerun callback is held by every
    /// upstream).
    pub(crate) fn register_listener_rc(&self, callback: Rc<dyn Fn()>) -> u64 {
        let id = self.next_listener_id.get();
        self.next_listener_id.set(id.wrapping_add(1));
        self.listeners.borrow_mut().push((id, callback));
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

impl<Row: 'static> crate::selector::AnyTrackable for QuerySubscription<Row> {
    fn register_listener(&self, callback: Rc<dyn Fn()>) -> u64 {
        self.register_listener_rc(callback)
    }

    fn unregister_listener(&self, id: u64) {
        // Disambiguate from the trait method we're inside via UFCS.
        QuerySubscription::<Row>::unregister_listener(self, id);
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

    fn driver_epoch(&self) -> DriverEpoch {
        self.epoch.clone()
    }

    fn driver_spawned(&self) -> bool {
        self.driver_spawned.get()
    }

    fn set_driver_spawned(&self) {
        self.driver_spawned.set(true);
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
pub(crate) struct QueryClientInner {
    endpoint: String,
    /// Driver configuration (poll interval, live-wakeup flag,
    /// credentials). `None` means "no driver" — `observe` skips the
    /// spawn step. Set by `QueryClient::without_driver`.
    pub(crate) config: Option<QueryClientConfig>,
    /// Active subscriptions keyed by `(Row TypeId, QueryKey)`. Two
    /// queries with the same `(stream, params, order, limit)` but
    /// different `Row` types live in separate entries — no
    /// `Rc<dyn Any>` downcast can panic on a Row mismatch.
    registry: RefCell<HashMap<RegistryKey, Rc<dyn AnyQuerySubscription>>>,
    /// Mutations that failed with a transport error and need to be
    /// re-fired by the driver on the next successful tick. FIFO so
    /// the original order is preserved on replay. The driver
    /// dequeues the whole batch on each tick and re-inserts
    /// entries that are still offline.
    pub(crate) replay_queue: RefCell<VecDeque<ReplayEntry>>,
    /// Registered `Mutator` impls, keyed by
    /// `(Mutator::STREAM, Mutator::NAME)` so two streams sharing a
    /// common mutator name (e.g. `"create"`) don't overwrite each
    /// other. Used by the driver's Phase 0 hydrate path to replay
    /// persisted `ClientMutation<Value>` payloads against the
    /// matching `Mutator::apply_remote`.
    ///
    /// Apps that don't enable persistence never touch this map.
    /// Mutators register via [`QueryClient::register_mutator`].
    pub(crate) mutators: RefCell<MutatorRegistry>,
    /// Selector registry, keyed by `(SelectorId, args_hash)`. Each
    /// slot is a small bucket so two distinct arg values whose
    /// `Hash` outputs collide can coexist — the lookup
    /// disambiguates with `AnyArgs::dyn_eq`. Without this, a hash
    /// collision would return the wrong cached value to the second
    /// caller.
    ///
    /// Holds `Weak` so an entry whose last `SelectorView` and last
    /// parent-selector tracker dropped is GC'd via the entry's
    /// `Drop` impl (which clears its own dead Weak from the bucket).
    /// Strong references live on `SelectorView`s and inside parent
    /// selectors' captured-dep keepers.
    pub(crate) selectors: RefCell<HashMap<(SelectorId, u64), SelectorBucket>>,
}

/// Type alias for the mutator registry — `clippy::type_complexity`
/// silenced by extraction.
type MutatorRegistry = HashMap<(&'static str, &'static str), Rc<dyn AnyMutator>>;

pub struct QueryClient {
    inner: Rc<QueryClientInner>,
}

impl Clone for QueryClient {
    /// Cheap — clones the inner `Rc`. Two clones share the same
    /// subscription + selector registry, so a clone passed into a
    /// `#[query]` selector's compute closure observes the same
    /// reactive state as the outer caller.
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl QueryClient {
    /// Build a fresh client with default config (driver enabled,
    /// 30s poll cadence, framework endpoint).
    ///
    /// **Native runtime requirement.** When this client's driver
    /// runs (via the first [`observe`](Self::observe) call), it
    /// spawns the per-subscription task with
    /// `tokio::task::spawn_local`. The caller's tokio runtime
    /// must therefore have an active
    /// [`tokio::task::LocalSet`](https://docs.rs/tokio/latest/tokio/task/struct.LocalSet.html);
    /// tests should wrap their body in
    /// `LocalSet::new().run_until(async { ... }).await`. Wasm
    /// builds use `wasm_bindgen_futures::spawn_local` and have no
    /// equivalent constraint. If you only want the routing engine
    /// — no spawned drivers — use [`without_driver`](Self::without_driver).
    pub fn new() -> Self {
        Self::with_config(QueryClientConfig::default())
    }

    /// Build a client targeting a custom endpoint prefix. Useful for
    /// tests against a router mounted at a different path. The
    /// other config knobs stay at their defaults.
    pub fn with_endpoint(endpoint: String) -> Self {
        let cfg = QueryClientConfig {
            endpoint,
            ..QueryClientConfig::default()
        };
        Self::with_config(cfg)
    }

    /// Build a client with a fully-specified [`QueryClientConfig`].
    /// The driver is spawned on the first [`observe`](Self::observe)
    /// for each subscription — see [`Self::new`] for the native
    /// LocalSet requirement.
    pub fn with_config(config: QueryClientConfig) -> Self {
        let endpoint = config.endpoint.clone();
        Self {
            inner: Rc::new(QueryClientInner {
                endpoint,
                config: Some(config),
                registry: RefCell::new(HashMap::new()),
                replay_queue: RefCell::new(VecDeque::new()),
                mutators: RefCell::new(HashMap::new()),
                selectors: RefCell::new(HashMap::new()),
            }),
        }
    }

    /// Build a client with the driver disabled. Pure routing-engine
    /// mode — `observe` returns a view whose `canonical_rows`
    /// stays empty until the caller manually feeds it through
    /// `mutate`. Used by tests that want to exercise the routing
    /// engine without spawning background tasks.
    pub fn without_driver() -> Self {
        let endpoint = SYNC_ENDPOINT_PREFIX.to_string();
        Self {
            inner: Rc::new(QueryClientInner {
                endpoint,
                config: None,
                registry: RefCell::new(HashMap::new()),
                replay_queue: RefCell::new(VecDeque::new()),
                mutators: RefCell::new(HashMap::new()),
                selectors: RefCell::new(HashMap::new()),
            }),
        }
    }

    /// Re-wrap an existing [`QueryClientInner`] so the driver's
    /// replay path can call `mutate` against the canonical
    /// `QueryClient` shape. Crate-internal helper.
    pub(crate) fn from_inner(inner: Rc<QueryClientInner>) -> Self {
        Self { inner }
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
    ///
    /// When the underlying client has a [`QueryClientConfig`]
    /// installed (the default; cleared by
    /// [`without_driver`](Self::without_driver)), this is also the
    /// moment a per-subscription driver task is spawned. The
    /// driver issues `/open` + initial `/pull` immediately, then
    /// loops on the configured poll interval — per RFC 087 §1.
    ///
    /// Spawn happens once per subscription identity; observing
    /// the same `Query` twice doesn't re-spawn.
    pub fn observe<Row>(&self, query: Query<Row>) -> QueryView<Row>
    where
        Row: Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        let handle = self.subscribe(query);
        self.maybe_spawn_driver(&handle.subscription);
        QueryView { handle }
    }

    /// Runtime entry point for the `#[query]` macro. Users normally
    /// don't call this directly; the macro's generated
    /// `<fn_name>::observe(client, args)` desugars to this call.
    ///
    /// * `id` — stable selector identity (from the macro).
    /// * `args_hash` — caller-computed hash of the args tuple.
    /// * `args` — type-erased copy of the args. The lookup
    ///   disambiguates hash collisions by equality on this value,
    ///   so two distinct arg values whose hashes collide can't
    ///   alias each other's cache entries.
    /// * `compute` — closure that runs the user fn body with the args
    ///   captured. Called once per rerun.
    ///
    /// On a cache hit (entry with matching `(id, args_hash)` AND
    /// `args.dyn_eq(...)` already lives in the selector registry),
    /// returns a fresh [`SelectorView`](crate::SelectorView)
    /// wrapping the existing entry; `compute` is NOT invoked.
    ///
    /// On a cache miss, builds the entry, runs `compute` once inside
    /// a tracking frame, captures the read deps + wires listeners,
    /// caches the output, and returns the view. The entry stays in
    /// the registry until its last strong reference (`SelectorView`
    /// plus any parent selector's captured-dep keeper) drops.
    pub fn observe_selector<T, F>(
        &self,
        id: SelectorId,
        args_hash: u64,
        args: Box<dyn AnyArgs>,
        compute: F,
    ) -> SelectorView<T>
    where
        T: PartialEq + Clone + 'static,
        F: Fn() -> T + 'static,
    {
        let key = (id, args_hash);

        // Cache hit path. Walk the bucket; for each live `Weak`,
        // check args equality AND that the entry's concrete type
        // downcasts to the caller's `T`. Either failed dyn_eq or
        // failed downcast means "not our entry — keep looking" (no
        // panic).
        //
        // The downcast filter is the principled escape from FNV-1a
        // 64-bit `SelectorId` collisions: if two `#[query]` fns ever
        // happen to hash to the same id and one is queried first,
        // the wrong-T entry is in the bucket. Filtering by downcast
        // lets the right-T entry be found, OR — if absent — falls
        // through to the cache-miss path that builds a fresh entry.
        // Dead `Weak`s are tolerated here too; they get cleaned up
        // by the entry's `Drop`.
        if let Some(typed) = {
            let map = self.inner.selectors.borrow();
            map.get(&key).and_then(|bucket| {
                bucket.iter().find_map(|w| {
                    let alive = w.upgrade()?;
                    if !alive.args().dyn_eq(&*args) {
                        return None;
                    }
                    Rc::downcast::<SelectorEntry<T>>(alive.as_rc_any()).ok()
                })
            })
        } {
            return SelectorView::from_entry(typed);
        }

        // Cache miss. Build, insert, run first rerun, return.
        let entry = Rc::new(SelectorEntry::<T>::new(
            id,
            args_hash,
            args,
            Box::new(compute),
            Rc::downgrade(&self.inner),
        ));
        {
            let mut map = self.inner.selectors.borrow_mut();
            let bucket = map.entry(key).or_default();
            // Opportunistic GC: prune any dead Weaks already in the
            // bucket. Keeps bucket size bounded under churn.
            bucket.retain(|w| w.strong_count() > 0);
            bucket.push(Rc::downgrade(&entry) as Weak<dyn AnySelector>);
        }
        entry.rerun();
        SelectorView::from_entry(entry)
    }

    /// Spawn the driver for `subscription` if (1) the client has a
    /// config and (2) the subscription doesn't already have a
    /// driver. Idempotent.
    fn maybe_spawn_driver<Row>(&self, subscription: &Rc<QuerySubscription<Row>>)
    where
        Row: Clone + serde::Serialize + serde::de::DeserializeOwned + 'static,
    {
        let Some(config) = self.inner.config.as_ref() else {
            return;
        };
        if subscription.driver_spawned() {
            return;
        }
        let epoch = subscription.epoch.clone();
        let weak_sub = Rc::downgrade(subscription);
        let weak_inner = Rc::downgrade(&self.inner);
        let driver = SubscriptionDriver::<Row>::new(weak_sub, weak_inner, epoch, config);
        subscription.set_driver_spawned();
        spawn_driver(driver.run());
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
    /// RFC 090 Phase 4 — push a typed mutation built by the macro-
    /// emitted `Issue::create(...)` / `update(...)` / `delete(...)`
    /// methods. Thin wrapper around [`Self::push`] that destructures
    /// the [`crate::write::TypedMutation`] builder, runs the
    /// optimistic-row closure (if any), and forwards.
    pub async fn push_typed<Row, Id, Draft>(
        &self,
        stream: SyncStreamName,
        mutation_id: MutationId,
        mutation: crate::write::TypedMutation<Row, Id, Draft>,
        push_url: &str,
    ) -> pocopine_sync::SyncResult<()>
    where
        Row: Clone + serde::Serialize + 'static,
        Id: serde::Serialize + Clone + 'static,
        Draft: serde::Serialize + Clone + 'static,
    {
        let wire_row_key = mutation.wire_row_key().cloned();
        let (payload, optimistic_closure) = mutation.into_parts();
        match optimistic_closure {
            Some(build) => {
                let row = build(&payload);
                let change = RowChange::Upsert(row);
                self.push(stream, mutation_id, payload, change, push_url)
                    .await
            }
            None => {
                // No optimistic. Skip the routing engine but still
                // build the wire envelope correctly: SourceResource::push
                // requires the wire `op` to match the payload's
                // `sync_op()` AND the wire `key` to match the
                // payload's id. Review-of-review #2/#8: the macro
                // pre-computes the wire key from `id.to_string()` at
                // construction, so non-String ids (i64, Uuid, …) flow
                // through correctly. Falling back to None for hand-
                // built `TypedMutation`s without `with_wire_row_key`
                // matches the server-side "missing key" rejection,
                // surfacing the omission instead of silently shipping
                // a mismatched mutation.
                let payload_value = serde_json::to_value(&payload)
                    .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;
                let mut wire = pocopine_sync::ClientMutation::new(
                    mutation_id,
                    payload.sync_op(),
                    payload_value,
                );
                wire.key = wire_row_key;
                let request = pocopine_sync::SyncPushRequest::new(stream, [wire]);
                pocopine_core::fetch::call::<
                    pocopine_sync::SyncPushRequest<Value>,
                    pocopine_sync::SyncPushResponse<Value>,
                >(push_url, &request)
                .await
                .map(|_| ())
                .map_err(|e| {
                    pocopine_sync::SyncError::backend(format!("push transport failed: {e}"))
                })
            }
        }
    }

    /// RFC 090 Phase 3 — high-level client-side push that pairs with
    /// `SourceResource::push` on the server. Use when you have a
    /// typed `MutationPayload` (or any `Serialize` envelope) and one
    /// optimistic `RowChange` to apply locally.
    ///
    /// Differs from [`Self::mutate`] in two ways:
    /// 1. No `Mutator` trait required — callers build payload + change
    ///    explicitly (the typed `Issue::create(draft)` API in Phase 4
    ///    will be a thin wrapper over this method).
    /// 2. No offline-replay queue. Network errors are surfaced to the
    ///    caller as `Err(SyncError::Transport(...))` and the
    ///    optimistic overlay is rolled back. Callers that need
    ///    offline replay use `mutate` with a `Mutator` impl.
    ///
    /// Lifecycle:
    /// 1. Apply optimistic locally — `change` is routed through every
    ///    active subscription's predicate matcher; matching views
    ///    fire `on_update` immediately.
    /// 2. POST `/sync/v1/push` with one `ClientMutation<Value>`.
    /// 3. Success → no rollback (the next `/pull` confirms canonical
    ///    state). Failure → roll back the optimistic overlay.
    ///
    /// The caller is responsible for the `mutation_id`. Typically use
    /// `pocopine_sync::ClientMutationDraft` and `.with_id()` to
    /// derive one from a durable counter.
    pub async fn push<P, Row>(
        &self,
        stream: SyncStreamName,
        mutation_id: MutationId,
        payload: P,
        change: RowChange<Row>,
        push_url: &str,
    ) -> pocopine_sync::SyncResult<()>
    where
        P: serde::Serialize,
        Row: Clone + serde::Serialize + 'static,
    {
        let payload_value = serde_json::to_value(&payload)
            .map_err(|e| pocopine_sync::SyncError::client(e.to_string()))?;

        let wire_op = match &change {
            RowChange::Upsert(_) => pocopine_sync::SyncOp::Upsert,
            RowChange::Delete(_) => pocopine_sync::SyncOp::Delete,
        };
        let row_key = match &change {
            RowChange::Upsert(row) => row_key_of(row),
            RowChange::Delete(key) => Some(key.clone()),
        };
        let mut wire_mutation =
            pocopine_sync::ClientMutation::new(mutation_id.clone(), wire_op, payload_value.clone());
        wire_mutation.key = row_key;

        // 1. Optimistic apply.
        self.route_optimistic_changes::<Row>(
            &stream,
            &mutation_id,
            &wire_mutation,
            std::slice::from_ref(&change),
        );

        // 2. Arm a rollback guard BEFORE the await. Review-of-review
        // finding #10: the previous version had three separate
        // `dequeue_pending_for_stream` calls along the error paths,
        // each easy to forget when a new branch is added (and to
        // skip if the future is dropped mid-await). The guard
        // unconditionally rolls back on Drop; only the accepted-
        // success path calls `disarm()`.
        let guard: RollbackGuard<'_, Row> = RollbackGuard {
            client: Some(self),
            stream: stream.clone(),
            mutation_id: mutation_id.clone(),
            _row: std::marker::PhantomData,
        };

        // 3. Wire push.
        let request = pocopine_sync::SyncPushRequest::new(stream.clone(), [wire_mutation.clone()]);
        let result = pocopine_core::fetch::call::<
            pocopine_sync::SyncPushRequest<Value>,
            pocopine_sync::SyncPushResponse<Value>,
        >(push_url, &request)
        .await;

        match result {
            Ok(response) => {
                // RFC 090 review #4: HTTP 200 doesn't mean the
                // server accepted the mutation. SourceResource
                // returns 200 with `rejected` / `conflicts` arrays
                // populated when the server short-circuits (bad
                // payload, version mismatch, etc.). The live
                // wakeup only fires for ACCEPTED mutations, so a
                // rejected/conflicted push leaves the optimistic
                // overlay alive indefinitely unless the guard
                // rolls it back.
                let mutation_id_accepted = response.accepted.iter().any(|id| id == &mutation_id);
                let mutation_id_rejected_or_conflicted = response
                    .rejected
                    .iter()
                    .any(|r| r.mutation_id == mutation_id)
                    || response
                        .conflicts
                        .iter()
                        .any(|c| c.mutation_id == mutation_id);
                if mutation_id_accepted {
                    // Success — overlay stays in place until the
                    // next /pull swaps it for canonical.
                    guard.disarm();
                    Ok(())
                } else if mutation_id_rejected_or_conflicted {
                    // Guard rolls back on drop.
                    Err(pocopine_sync::SyncError::backend(
                        "server rejected the mutation; optimistic overlay rolled back",
                    ))
                } else {
                    // The server didn't mention our mutation id at
                    // all. Treat as a transport-level failure;
                    // guard rolls back defensively.
                    Err(pocopine_sync::SyncError::backend(
                        "server response did not include our mutation; rolled back",
                    ))
                }
            }
            Err(err) => {
                // Guard rolls back on drop.
                Err(pocopine_sync::SyncError::backend(format!(
                    "push transport failed: {err}"
                )))
            }
        }
    }

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
            pocopine_sync::ClientMutation::new(mutation_id.clone(), wire_op, payload_value.clone());

        self.route_optimistic_changes::<M::Row>(
            &stream,
            &mutation_id,
            &wire_mutation,
            &local_changes,
        );

        // Arm a cancellation guard BEFORE the persist .await so a
        // future dropped mid-persist still rolls back the optimistic
        // overlay. Without this, IndexedDB / SQLite yields inside
        // persist_pending_mutation could leave a pending overlay
        // alive across cancellation even though apply_remote never
        // ran.
        let guard: RollbackGuard<'_, M::Row> = RollbackGuard {
            client: Some(self),
            stream: stream.clone(),
            mutation_id: mutation_id.clone(),
            _row: std::marker::PhantomData,
        };

        // Persist the pending mutation immediately (RFC 088 §A.5)
        // so a reload BEFORE the server response can still replay
        // the mutation on the next online tick. We wrap the
        // payload in the `{__mutator, __payload}` envelope so the
        // hydrate path can pick the right Mutator impl by NAME.
        // The on-wire mutation (sent to the server below) carries
        // the BARE payload — the envelope is store-only.
        self.persist_pending_mutation::<M>(
            &stream,
            &mutation_id,
            wire_op,
            &payload_value,
            &local_changes,
        )
        .await;

        // Capture the original push URL + mutation_id so an
        // offline replay can rebuild a `MutatorRemoteContext`
        // that surfaces the SAME identity on retry — preserving
        // the server-side dedup contract (RFC 087 §7 / RFC 072).
        let original_push_url = ctx.push_url().to_string();

        // 2. Wire push (caller-supplied context handles transport).
        let canonical_changes = match M::apply_remote(ctx, payload).await {
            Ok(c) => c,
            Err(err) => {
                if err.is_transport() && self.inner.config.is_some() {
                    // RFC 087 §7 offline path: keep the optimistic
                    // overlay in pending + enqueue a replay for the
                    // driver to fire on the next successful tick.
                    guard.disarm();
                    let entry = build_replay_entry::<M>(
                        stream.clone(),
                        mutation_id.clone(),
                        original_push_url,
                        wire_mutation.payload.clone(),
                    );
                    self.inner.replay_queue.borrow_mut().push_back(entry);
                    tracing::info!(
                        target: "pocopine.log",
                        stream = stream.as_str(),
                        mutation_id = %mutation_id.as_str(),
                        "sync-query: apply_remote network error; queued for replay"
                    );
                    return Err(err);
                }
                // Non-transport error OR driver disabled — fall
                // through to the guard's Drop, which rolls back.
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

        // Clear the durable pending entry from every compartment
        // we wrote it into. Without this, an accepted mutation
        // would persist forever and re-fire on every subsequent
        // reload (the server's mutation_id dedup would no-op the
        // replay, but the overlay would still bounce through the
        // pending state machine every session — wasted bandwidth +
        // confusing UX).
        self.clear_persisted_pending::<M::Row>(&stream, &mutation_id)
            .await;

        Ok(crate::MutationOutcome::Accepted(canonical_changes))
    }

    /// Clear a durable pending mutation across every compartment
    /// of `stream` after the canonical reconcile confirmed it.
    /// Uses `mark_push_result` (which the store contract guarantees
    /// removes accepted ids from the pending queue) so the
    /// implementation works against every `SyncLocalStore` impl
    /// without depending on a future purge-by-id primitive.
    async fn clear_persisted_pending<Row>(&self, stream: &SyncStreamName, mutation_id: &MutationId)
    where
        Row: 'static,
    {
        let Some(config) = self.inner.config.as_ref() else {
            return;
        };
        let Some(store) = config.local_store.clone() else {
            return;
        };
        // Walk every compartment we may have written into:
        // each currently-active subscription's compartment plus
        // the bare-stream fallback. We over-clear (some
        // compartments may not contain the entry) — that's safe:
        // `mark_push_result` is a no-op for a missing id.
        let mut compartments: std::collections::BTreeSet<SyncStreamName> =
            std::collections::BTreeSet::new();
        for typed in self.collect_subscriptions_on_stream::<Row>(stream) {
            compartments.insert(pocopine_sync::local_stream_key(
                stream,
                typed.query.params(),
            ));
        }
        compartments.insert(stream.clone());
        for compartment in compartments {
            let mut result = pocopine_sync::LocalPushResult {
                stream: compartment.clone(),
                collection: None,
                accepted: vec![mutation_id.clone()],
                rejected: Vec::new(),
                rows: Vec::new(),
                conflicts: Vec::new(),
                cursor: None,
            };
            // Some stores expect a collection on every result.
            // MemoryLocalStore tolerates None — the SQLite +
            // IndexedDB impls also handle it (collection is
            // informational on the local store). Leave it None
            // so we don't fabricate a name.
            let _ = &mut result;
            if let Err(err) = store.mark_push_result(result).await {
                tracing::warn!(
                    target: "pocopine.log",
                    stream = compartment.as_str(),
                    mutation_id = %mutation_id,
                    error = %err,
                    "sync-query: mark_push_result on accepted mutation failed; \
                     the entry may replay on next hydrate (server dedup will no-op)"
                );
            }
        }
    }

    /// Persist a freshly-created pending mutation to the durable
    /// local store (RFC 088 §A.5). Wraps the wire payload in a
    /// `{__mutator, __payload}` envelope so the hydrate path can
    /// pick the right `Mutator` impl by NAME without bloating the
    /// wire `ClientMutation` shape.
    ///
    /// **Compartment fan-out.** A single mutation can affect
    /// multiple subscriptions on the same stream that filter on
    /// different params (e.g. two `Issue::query()` views over
    /// different `workspace_id`s — one mutation creates an issue
    /// that matches workspace A's predicate). To make Phase 0
    /// hydrate restore the same fan-out, we persist the pending
    /// envelope INTO EACH affected subscription's compartment
    /// AND into the bare-stream compartment as a fallback for
    /// subscriptions that no longer have an open view at mutate
    /// time but might reopen later.
    ///
    /// No-op when the client has no `local_store` configured —
    /// matches the in-memory-only behavior of the historical
    /// surface.
    async fn persist_pending_mutation<M>(
        &self,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
        wire_op: pocopine_sync::SyncOp,
        payload_value: &Value,
        local_changes: &[RowChange<M::Row>],
    ) where
        M: crate::Mutator,
        M::Row: Clone + serde::Serialize + 'static,
    {
        let Some(config) = self.inner.config.as_ref() else {
            return;
        };
        let Some(store) = config.local_store.clone() else {
            return;
        };
        // Build the persistence-only `ClientMutation<Value>` that
        // carries the mutator NAME envelope. The on-the-wire
        // mutation already routed through `route_optimistic_changes`
        // is unaffected (it carries the bare payload for the
        // server).
        let envelope = wrap_persisted_payload(M::NAME, payload_value.clone());
        let persisted = ClientMutation::new(mutation_id.clone(), wire_op, envelope);
        // Try to find the optimistic row to capture for restore.
        // Multiple changes may match — capture the first Upsert
        // (Delete-only mutations leave optimistic_row None, matching
        // the in-memory PendingOverlay shape).
        let optimistic_row = local_changes.iter().find_map(|change| match change {
            RowChange::Upsert(row) => {
                let value = serde_json::to_value(row).ok()?;
                let id = value.get("id").and_then(|v| v.as_str())?.to_string();
                let key = RowKey::new(id).ok()?;
                Some(SyncRow {
                    key,
                    version: None,
                    value,
                    pending: true,
                    conflict: false,
                })
            }
            RowChange::Delete(_) => None,
        });
        // Walk every subscription on the stream that received the
        // optimistic upsert (or delete) and persist into its
        // compartment. The routing engine already gated visibility
        // by predicate; we mirror the same gate by checking
        // `state.pending()` for a matching mutation_id.
        let mut compartments: std::collections::BTreeSet<SyncStreamName> =
            std::collections::BTreeSet::new();
        for typed in self.collect_subscriptions_on_stream::<M::Row>(stream) {
            let state = typed.state.borrow();
            let has_overlay = state
                .pending()
                .iter()
                .any(|o| &o.mutation_id == mutation_id);
            if has_overlay {
                compartments.insert(pocopine_sync::local_stream_key(
                    stream,
                    typed.query.params(),
                ));
            }
        }
        if compartments.is_empty() {
            // No subscription matched. Persist into the bare-stream
            // compartment as a fallback so a subscription opened
            // AFTER the page reloads still picks up the queue.
            // This matches the routing engine's invariant: a
            // mutation produced canonical changes that the server
            // will accept; on hydrate the bare compartment is
            // consulted by every newly-opened subscription via the
            // bare-fallback path documented on `enqueue_hydrated_pending`.
            compartments.insert(stream.clone());
        }
        for compartment in compartments {
            let pending = pocopine_sync::LocalPendingMutation::new(persisted.clone())
                .with_optimistic_row(optimistic_row.clone());
            if let Err(err) = store.enqueue_pending_mutation(&compartment, pending).await {
                tracing::warn!(
                    target: "pocopine.log",
                    stream = compartment.as_str(),
                    mutation_id = %mutation_id,
                    error = %err,
                    "sync-query: enqueue_pending_mutation failed; reload will not replay this mutation"
                );
            }
        }
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

    /// Route a server-confirmed canonical pull into matching
    /// subscriptions. Driver-only entry point — distinct from
    /// [`route_canonical_changes`] because `/pull` results carry
    /// no `mutation_id` (they're unsolicited canonical updates
    /// from the server, not the canonical-side of a client
    /// mutation).
    ///
    /// Semantics:
    ///
    /// * `Upsert(row)` — for each subscription on `stream`:
    ///   * predicate matches AND row not in canonical → upsert.
    ///   * predicate matches AND row already in canonical → upsert
    ///     (refresh the cached value).
    ///   * predicate doesn't match AND row in canonical → remove
    ///     (server-confirmed predicate departure).
    ///   * predicate doesn't match AND row not in canonical → no-op.
    /// * `Delete(key)` — remove from every subscription's
    ///   canonical that holds the key.
    ///
    /// Does NOT touch the pending overlay — overlays are the
    /// mutation-driven path's responsibility; the driver is only
    /// authoritative on canonical state. Pending overlays for a
    /// matching key remain visible; the merge in `QueryView::rows`
    /// makes the optimistic upsert win over the canonical until
    /// the matching `mutate()` clears it.
    pub(crate) fn route_canonical_pull<Row>(
        inner: &Rc<QueryClientInner>,
        stream: &SyncStreamName,
        canonical: &[RowChange<Row>],
    ) where
        Row: Clone + serde::Serialize + 'static,
    {
        let subscriptions = collect_subscriptions_on_stream_inner::<Row>(inner, stream);
        for typed in subscriptions {
            let touched = {
                let mut state = typed.state.borrow_mut();
                let mut touched = false;
                for change in canonical {
                    match change {
                        RowChange::Upsert(row) => {
                            let Some(key) = row_key_of(row) else {
                                continue;
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
                            } else if state.canonical_contains(&key) {
                                state.remove_canonical(&key);
                                touched = true;
                            }
                        }
                        RowChange::Delete(key) => {
                            if state.canonical_contains(key) {
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

    /// Wipe canonical rows on every subscription on `stream` whose
    /// `Row` type matches. Retained for future stream-wide reset
    /// scenarios (e.g., admin-initiated schema rollover across
    /// every subscription on a stream). The driver's per-pull
    /// snapshot path uses a per-subscription wipe inside
    /// `apply_pull` instead — different subscriptions are
    /// independent compartments and a stream-wide wipe would
    /// clobber a subscription whose /pull hasn't run yet.
    ///
    /// Notifies listeners only when the canonical set was actually
    /// non-empty — avoids gratuitous re-renders for a snapshot pull
    /// that lands on a fresh subscription.
    #[allow(dead_code)]
    pub(crate) fn clear_canonical_on_stream<Row>(
        inner: &Rc<QueryClientInner>,
        stream: &SyncStreamName,
    ) where
        Row: Clone + 'static,
    {
        let subscriptions = collect_subscriptions_on_stream_inner::<Row>(inner, stream);
        for typed in subscriptions {
            let touched = {
                let mut state = typed.state.borrow_mut();
                let keys: Vec<RowKey> = state.canonical_rows().map(|r| r.key.clone()).collect();
                let was_non_empty = !keys.is_empty();
                for key in keys {
                    state.remove_canonical(&key);
                }
                was_non_empty
            };
            if touched {
                typed.notify_listeners();
            }
        }
    }

    /// Driver-only: drain the replay queue. The caller iterates the
    /// returned `Vec` and pushes still-failing entries back via
    /// [`reinsert_replay_entry`].
    pub(crate) fn take_replay_entries(inner: &Rc<QueryClientInner>) -> Vec<ReplayEntry> {
        let mut q = inner.replay_queue.borrow_mut();
        q.drain(..).collect()
    }

    /// Register a [`crate::Mutator`] impl so the hydrate path can
    /// recover its replay closure. Idempotent — re-registering the
    /// same `Mutator::NAME` replaces the prior entry (last-write-wins;
    /// the lookup keys on `Mutator::NAME`, not `TypeId`, because two
    /// runs of the same mutator type in different code paths must
    /// still resolve to one entry).
    ///
    /// Mutators should self-register at construction time — see
    /// `examples/sync-query/src/lib.rs` for the cookbook shape.
    pub fn register_mutator<M>(&self)
    where
        M: crate::Mutator,
        M::Row: Clone + serde::Serialize + 'static,
    {
        let entry: Rc<dyn AnyMutator> = Rc::new(MutatorEntry::<M>::new());
        self.inner
            .mutators
            .borrow_mut()
            .insert((M::STREAM, M::NAME), entry);
    }

    /// Driver-only: enqueue hydrated pending mutations for replay
    /// through the registered [`AnyMutator`] entries. Called from
    /// [`SubscriptionDriver::apply_hydrated_snapshot`] AFTER the
    /// in-memory `PendingOverlay`s are pushed onto the
    /// subscription — so a fresh observer sees the optimistic
    /// rows immediately, then the replay tick either confirms
    /// them via canonical reconcile or rolls them back via
    /// `dequeue_pending_for_stream`.
    pub(crate) fn enqueue_hydrated_pending(
        inner: &Rc<QueryClientInner>,
        stream: SyncStreamName,
        mutations: Vec<ClientMutation<Value>>,
    ) {
        if mutations.is_empty() {
            return;
        }
        let endpoint = inner.endpoint.clone();
        // Snapshot the registered mutator map. Resolving via NAME
        // happens lazily inside the replay closure so a mutator
        // registered AFTER hydrate still fires (the driver-side
        // hydrate phase runs once per spawn).
        let weak_inner = Rc::downgrade(inner);
        for mutation in mutations {
            let stream_clone = stream.clone();
            let push_url = build_push_url(&endpoint);
            let weak_inner_clone = weak_inner.clone();
            let mutation_for_closure = mutation.clone();
            let mutation_id_for_entry = mutation_id_from_value(&mutation);
            let replay: crate::driver::ReplayFuture = Box::new(move |client: &QueryClient| {
                let stream = stream_clone.clone();
                let mutation = mutation_for_closure.clone();
                let push_url = push_url.clone();
                let weak_inner = weak_inner_clone.clone();
                Box::pin(async move {
                    let Some(client_inner) = weak_inner.upgrade() else {
                        // Client gone; report as Rejected so the
                        // replay queue drops the entry instead of
                        // re-queueing.
                        return ReplayOutcome::Rejected;
                    };
                    // Recover the mutator NAME from the persisted
                    // envelope so we can pick the right Mutator
                    // impl. Falls back to the raw payload when the
                    // entry was written before the envelope landed
                    // — in that case we have no NAME and must
                    // reject (the registered NAME -> AnyMutator
                    // map is the only resolution path).
                    let mutator_name = match crate::mutator::unwrap_persisted_payload(
                        &mutation.payload,
                    ) {
                        Some((name, _)) => name,
                        None => {
                            tracing::warn!(
                                target: "pocopine.log",
                                stream = stream.as_str(),
                                mutation_id = %mutation.id,
                                "sync-query: hydrated mutation missing mutator name envelope; dropping"
                            );
                            Self::dequeue_pending_for_stream::<()>(
                                &client_inner,
                                &stream,
                                &mutation.id,
                            );
                            return ReplayOutcome::Rejected;
                        }
                    };
                    // Lookup by (stream, NAME) so two streams that
                    // register a common mutator name like `create`
                    // don't overwrite each other (codex P1).
                    let entry = client_inner
                        .mutators
                        .borrow()
                        .iter()
                        .find(|((entry_stream, entry_name), _)| {
                            *entry_stream == stream.as_str() && *entry_name == mutator_name.as_str()
                        })
                        .map(|(_, e)| e.clone());
                    let Some(entry) = entry else {
                        tracing::warn!(
                            target: "pocopine.log",
                            stream = stream.as_str(),
                            mutator_name = %mutator_name,
                            mutation_id = %mutation.id,
                            "sync-query: no Mutator registered for hydrated pending; dropping"
                        );
                        return ReplayOutcome::Rejected;
                    };
                    let outcome = entry
                        .replay_hydrated(client, stream.clone(), mutation.clone(), push_url)
                        .await;
                    match outcome {
                        HydrateReplayOutcome::Accepted => ReplayOutcome::Accepted,
                        HydrateReplayOutcome::StillOffline => ReplayOutcome::StillOffline,
                        HydrateReplayOutcome::Rejected(reason) => {
                            tracing::warn!(
                                target: "pocopine.log",
                                stream = stream.as_str(),
                                error = %reason,
                                "sync-query: hydrated replay rejected; rolling back overlay"
                            );
                            ReplayOutcome::Rejected
                        }
                    }
                })
            });
            inner.replay_queue.borrow_mut().push_back(ReplayEntry {
                stream: stream.clone(),
                mutation_id: mutation_id_for_entry,
                replay,
            });
        }
    }

    /// Driver-only: push a replay entry back on the queue (the
    /// caller's replay attempt observed `StillOffline`).
    pub(crate) fn reinsert_replay_entry(inner: &Rc<QueryClientInner>, entry: ReplayEntry) {
        inner.replay_queue.borrow_mut().push_back(entry);
    }

    /// Driver-only: roll back the optimistic overlay for `mutation_id`
    /// across EVERY matching subscription on `stream`. Same semantics
    /// as the private `dequeue_pending` method on `QueryClient` — the
    /// shape this is exposed for the offline-replay path which only
    /// holds the inner `Rc`, not the outer `QueryClient`.
    ///
    /// This is the correct entry point for the rejected-replay
    /// cleanup path because optimistic state for one mutation is
    /// fan-out across every subscription whose predicate matched at
    /// the original `apply_local` time. Cleaning only the driver's
    /// own subscription leaves stale pending state behind on every
    /// other view that received the same optimistic upsert.
    pub(crate) fn dequeue_pending_for_stream<Row>(
        inner: &Rc<QueryClientInner>,
        stream: &SyncStreamName,
        mutation_id: &MutationId,
    ) where
        Row: Clone + 'static,
    {
        for typed in collect_subscriptions_on_stream_inner::<Row>(inner, stream) {
            let removed = {
                let mut state = typed.state.borrow_mut();
                let overlays = state.remove_pending(mutation_id);
                // Same conservative-restore rule as the in-flight
                // mutate() rollback: only re-upsert when canonical
                // doesn't already hold a (potentially newer) row
                // for the key. Skipping the restore on collision
                // lets the newer canonical stand; the next /pull
                // will reconcile.
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

    /// Snapshot all subscriptions on a given stream whose Row type
    /// matches `Row`. The returned Vec holds `Rc` clones so callers
    /// can freely call back into the `QueryClient` from inside the
    /// iteration body — no `registry` borrow is held across the
    /// `notify_listeners` invocations downstream.
    fn collect_subscriptions_on_stream<Row: 'static>(
        &self,
        stream: &SyncStreamName,
    ) -> Vec<Rc<QuerySubscription<Row>>> {
        collect_subscriptions_on_stream_inner(&self.inner, stream)
    }
}

/// Resolve the push URL for a given configured endpoint prefix.
/// Mirrors `build_url` in `driver.rs` but for the push path —
/// kept short since the replay path needs it.
fn build_push_url(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.is_empty() {
        pocopine_sync::SYNC_PUSH_PATH.to_string()
    } else if let Some(suffix) =
        pocopine_sync::SYNC_PUSH_PATH.strip_prefix(pocopine_sync::SYNC_ENDPOINT_PREFIX)
    {
        format!("{trimmed}{suffix}")
    } else {
        pocopine_sync::SYNC_PUSH_PATH.to_string()
    }
}

/// Extract the mutation_id from a wire `ClientMutation`.
/// Helper kept inline for clarity — the wire shape has the id
/// directly on the envelope.
fn mutation_id_from_value(mutation: &ClientMutation<Value>) -> MutationId {
    mutation.id.clone()
}

/// Crate-internal route used by [`AnyMutator::replay_hydrated`] to
/// reconcile canonical changes through the same path as a successful
/// `mutate()` call. Necessary because the trait can't reach the
/// `pub(crate) route_canonical_changes` directly without re-exposing
/// the registry borrow path.
pub(crate) fn route_canonical_changes_from_replay<Row>(
    client: &QueryClient,
    stream: &SyncStreamName,
    mutation_id: &MutationId,
    canonical: &[crate::mutator::RowChange<Row>],
) where
    Row: Clone + serde::Serialize + 'static,
{
    client.route_canonical_changes::<Row>(stream, mutation_id, canonical);
}

/// Crate-internal route used by [`AnyMutator::replay_hydrated`] on
/// the Rejected path to clean up the pending overlay using the
/// MUTATOR'S row type (rather than the draining driver's row
/// type). Without this, a hydrated rejection routed through a
/// driver of a different row type would skip the matching
/// subscriptions and leak stale optimistic state.
pub(crate) fn dequeue_pending_for_mutator_row<Row>(
    client: &QueryClient,
    stream: &SyncStreamName,
    mutation_id: &MutationId,
) where
    Row: Clone + 'static,
{
    QueryClient::dequeue_pending_for_stream::<Row>(&client.inner, stream, mutation_id);
}

/// Free-function variant of `collect_subscriptions_on_stream` that
/// takes `Rc<QueryClientInner>` directly. Used by the driver's
/// canonical-pull / clear-canonical paths, which only hold the
/// inner `Rc` (not the outer `QueryClient`).
fn collect_subscriptions_on_stream_inner<Row: 'static>(
    inner: &Rc<QueryClientInner>,
    stream: &SyncStreamName,
) -> Vec<Rc<QuerySubscription<Row>>> {
    let target = TypeId::of::<Row>();
    let registry = inner.registry.borrow();
    registry
        .iter()
        .filter(|((tid, _), sub)| *tid == target && sub.stream() == stream)
        .filter_map(|(_, sub)| Rc::downcast::<QuerySubscription<Row>>(sub.clone().as_rc_any()).ok())
        .collect()
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

/// Context handed to a replayed `Mutator::apply_remote`. Surfaces
/// the ORIGINAL `mutation_id` (not a fresh one) so the server's
/// idempotency log accepts the retry as the same logical write —
/// per RFC 087 §7 and the wire dedup contract in RFC 072.
struct ReplayContext {
    mutation_id: MutationId,
    push_url: String,
}

impl crate::mutator::MutatorRemoteContext for ReplayContext {
    fn push_url(&self) -> &str {
        &self.push_url
    }

    fn next_mutation_id(&self) -> pocopine_sync::SyncResult<MutationId> {
        // The driver re-fires apply_remote with the same logical
        // identity. Clone instead of regenerate so the server
        // dedup path treats this as the same row.
        Ok(self.mutation_id.clone())
    }
}

/// Build the offline-replay closure for one mutator + captured
/// payload. The returned [`ReplayEntry`] is stored on the
/// `QueryClientInner::replay_queue` and re-fired by the driver on
/// the next successful tick.
///
/// Boxed as `Fn(&QueryClient) -> LocalBoxFuture` so the queue can
/// hold heterogeneous entries — different `Mutator` impls
/// monomorphize to different concrete types, but the wire-side
/// behavior collapses to the same trait-object shape.
fn build_replay_entry<M>(
    stream: SyncStreamName,
    mutation_id: MutationId,
    push_url: String,
    payload_value: Value,
) -> ReplayEntry
where
    M: crate::Mutator,
    M::Row: Clone + serde::Serialize + 'static,
{
    let replay_stream = stream.clone();
    let replay_mutation_id = mutation_id.clone();
    let replay_push_url = push_url.clone();
    let replay_payload = payload_value.clone();
    let replay: crate::driver::ReplayFuture = Box::new(move |client: &QueryClient| {
        let stream = replay_stream.clone();
        let mutation_id = replay_mutation_id.clone();
        let push_url = replay_push_url.clone();
        let payload_value = replay_payload.clone();
        let inner = client.inner.clone();
        Box::pin(async move {
            // Decode the captured wire payload back into the
            // mutator's typed Payload. A decode failure means
            // the user changed the payload shape between
            // submitting the mutation and the replay tick — we
            // treat that as Rejected (drop the pending; surface
            // via state.error on the next observe tick).
            let payload: M::Payload = match serde_json::from_value(payload_value) {
                Ok(p) => p,
                Err(err) => {
                    tracing::warn!(
                        target: "pocopine.log",
                        stream = stream.as_str(),
                        error = %err,
                        "sync-query: replay payload could not be decoded; dropping mutation"
                    );
                    return ReplayOutcome::Rejected;
                }
            };
            let ctx = ReplayContext {
                mutation_id: mutation_id.clone(),
                push_url,
            };
            match M::apply_remote(&ctx, payload).await {
                Ok(canonical_changes) => {
                    // Route into matching subscriptions; this also
                    // clears the pending overlay via remove_pending.
                    let client = QueryClient::from_inner(inner);
                    client.route_canonical_changes::<M::Row>(
                        &stream,
                        &mutation_id,
                        &canonical_changes,
                    );
                    ReplayOutcome::Accepted
                }
                Err(err) if err.is_transport() => ReplayOutcome::StillOffline,
                Err(err) => {
                    tracing::warn!(
                        target: "pocopine.log",
                        stream = stream.as_str(),
                        error = %err,
                        "sync-query: replay rejected by server; rolling back overlay"
                    );
                    ReplayOutcome::Rejected
                }
            }
        })
    });
    ReplayEntry {
        stream,
        mutation_id,
        replay,
    }
}

/// Bump the driver epoch for a subscription that just lost its
/// last `QueryHandle`. Crate-internal entry point so
/// `release_inner` doesn't need to know the trait-object shape.
fn maybe_bump_driver_epoch(sub: &Rc<dyn AnyQuerySubscription>) {
    sub.driver_epoch().bump();
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

impl<Row: 'static> Clone for QueryHandle<Row> {
    /// Bumps the subscription's refcount and clones the registry
    /// weak-ref. The new handle's `Drop` decrements just like the
    /// original. Used by [`QueryView::rows`] when running inside a
    /// selector body, so the selector entry can keep the underlying
    /// subscription alive between reruns via an erased keeper.
    fn clone(&self) -> Self {
        self.subscription.bump_refcount();
        Self {
            subscription: self.subscription.clone(),
            registry: self.registry.clone(),
        }
    }
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
        self.track_for_selector();
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
        self.track_for_selector();
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
        self.track_for_selector();
        // Cheap path: don't materialize rows just to ask "anything
        // visible?". A `rendered_key_count` walk visits each
        // canonical row and overlay once; no row cloning or JSON
        // serialization. The pending-empty check matches the
        // documented "idle view" semantics.
        self.rendered_key_count() == 0 && self.state().pending().is_empty()
    }

    /// Record this view's subscription as a dependency on the current
    /// selector's tracking frame, if one is active. No-op when no
    /// selector is running (ordinary component reads aren't tracked).
    ///
    /// The keeper is a cloned `QueryHandle`, which bumps the
    /// subscription's refcount and stays alive in the selector
    /// entry's `keepers` list until the selector reruns and the dep
    /// is no longer captured (or the selector itself is dropped).
    /// This is what prevents the subscription from being GC'd while
    /// a selector still observes it.
    fn track_for_selector(&self) {
        if !crate::selector::currently_tracking() {
            return;
        }
        let trackable: Rc<dyn crate::selector::AnyTrackable> = self.handle.subscription.clone();
        let keeper: Box<dyn std::any::Any> = Box::new(self.handle.clone());
        crate::selector::record_read(trackable, keeper);
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
        self.track_for_selector();
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
        self.track_for_selector();
        self.state().version()
    }

    /// Borrow the shared state for custom reactive integration.
    /// Same `Rc<RefCell<...>>` the routing engine writes through.
    pub fn shared_state(&self) -> Rc<RefCell<QueryState<Row>>> {
        self.track_for_selector();
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

impl Clone for RegistryWeakRef {
    /// Cheap — the `Weak` clone is one atomic ref bump and `key` is
    /// `Copy`. Used by [`QueryHandle::clone`] so a selector can carry
    /// its own handle into the entry's keepers without re-running
    /// `subscribe()`.
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            key: self.key,
        }
    }
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
    let Some(sub) = registry.get(&key).cloned() else {
        return;
    };
    if sub.decrement_refcount() == 0 {
        // Bump the driver epoch BEFORE removing the registry
        // entry — the spawned task may still hold a `Weak<inner>`
        // and re-enter; the epoch check at the next .await is
        // what guarantees it stops touching state.
        maybe_bump_driver_epoch(&sub);
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
