//! Selector layer for `pocopine-sync-query`.
//!
//! Implements the runtime behind `#[query]`-decorated functions: a
//! thread-local read-tracking stack, `(SelectorId, ArgsHash)`-keyed
//! memoization, invalidation that rides on the existing
//! `QuerySubscription::notify_listeners` plumbing, and `PartialEq`-diff
//! suppression of downstream notifications.
//!
//! See [`docs/sync-query-selector-mechanism.md`](../../docs/sync-query-selector-mechanism.md)
//! for the design explainer.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use crate::client::QueryClientInner;

/// Stable identity for a `#[query]`-decorated function.
///
/// Computed by the proc-macro at expansion time via FNV-1a 64 of a
/// crate-qualified identifier string. The macro is the only intended
/// producer of values; the `const` constructor exists for it (and for
/// unit tests that wire up a selector by hand).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct SelectorId(u64);

impl SelectorId {
    /// Build a `SelectorId` from a precomputed 64-bit hash. Used by
    /// the `#[query]` macro's expansion.
    pub const fn new(hash: u64) -> Self {
        Self(hash)
    }

    /// Raw 64-bit identity. Tests and tracing consumers only.
    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

/// Common interface every selector dependency implements. Both
/// `QuerySubscription<Row>` and [`SelectorEntry<T>`] impl it so the
/// tracking stack can hold them behind one `Rc<dyn AnyTrackable>`.
///
/// Object-safe — only `&self` methods, no associated types. The
/// trait is exposed publicly so that downstream crates implementing
/// custom reactive sources could plug into the selector tracker if
/// ever needed, but the in-tree impls are the canonical consumers.
pub trait AnyTrackable: 'static {
    /// Register a listener that fires after every state-mutating op
    /// on this trackable. Returns an id used by
    /// [`Self::unregister_listener`].
    fn register_listener(&self, callback: Rc<dyn Fn()>) -> u64;

    /// Drop a previously-registered listener.
    fn unregister_listener(&self, id: u64);
}

/// Type-erased selector arguments. The `#[query]` macro boxes the
/// args tuple at every `observe()` call so the cache can do an
/// equality check on lookup — NOT just a hash compare. Without this,
/// two distinct arg values whose `Hash` outputs happen to collide
/// would share one cached entry and the second caller would receive
/// the first call's cached value.
///
/// Implemented automatically for any `T: PartialEq + 'static` via a
/// blanket impl, so user code never names this trait directly.
pub trait AnyArgs: std::any::Any + 'static {
    /// `self == other` after a successful concrete-type downcast.
    /// Different concrete types compare `false`.
    fn dyn_eq(&self, other: &dyn AnyArgs) -> bool;
    /// Upcast to `&dyn Any` for safe downcasting in [`Self::dyn_eq`].
    fn as_any(&self) -> &dyn std::any::Any;
}

impl<T: PartialEq + 'static> AnyArgs for T {
    fn dyn_eq(&self, other: &dyn AnyArgs) -> bool {
        other
            .as_any()
            .downcast_ref::<T>()
            .is_some_and(|o| self == o)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// RAII handle returned by selector tracking. Drop unregisters the
/// listener from the upstream trackable; holds a `Weak` so a token
/// outliving its source silently no-ops.
pub struct TrackToken {
    trackable: Weak<dyn AnyTrackable>,
    id: u64,
}

impl TrackToken {
    pub(crate) fn new(trackable: &Rc<dyn AnyTrackable>, id: u64) -> Self {
        Self {
            trackable: Rc::downgrade(trackable),
            id,
        }
    }
}

impl Drop for TrackToken {
    fn drop(&mut self) {
        if let Some(t) = self.trackable.upgrade() {
            t.unregister_listener(self.id);
        }
    }
}

// ---- Thread-local tracking stack ----------------------------------

thread_local! {
    /// Stack of in-progress selector reruns. `record_read` pushes
    /// into the top frame; ordinary component reads (no frame active)
    /// no-op.
    static SELECTOR_STACK: RefCell<Vec<TrackingFrame>> = const { RefCell::new(Vec::new()) };
}

/// `(listener target, anything-to-keep-alive)` pair captured by the
/// tracking frame. The trackable is what listeners register against;
/// the keeper (a cloned `QueryHandle` or a nested
/// `Rc<SelectorEntry<T>>`) holds the upstream alive between reruns.
type TrackedDep = (Rc<dyn AnyTrackable>, Box<dyn Any>);

/// One frame on the tracking stack.
struct TrackingFrame {
    deps: Vec<TrackedDep>,
}

/// Record a dependency read against the current selector's tracking
/// frame. No-op when the stack is empty (the common case for
/// ordinary component reads outside any selector).
///
/// `keeper` is the value the selector entry stores to keep the
/// upstream alive across reruns — typically a cloned `QueryHandle`
/// for a subscription dep, or the upstream selector entry's `Rc` for
/// a nested-selector dep. Dedupe within a frame is by `Rc` pointer
/// identity, so multiple reads against the same view are folded into
/// one tracked dep.
pub(crate) fn record_read(trackable: Rc<dyn AnyTrackable>, keeper: Box<dyn Any>) {
    SELECTOR_STACK.with(|stack| {
        let mut stack = stack.borrow_mut();
        let Some(frame) = stack.last_mut() else {
            return;
        };
        let ptr = Rc::as_ptr(&trackable) as *const ();
        let already_tracked = frame
            .deps
            .iter()
            .any(|(t, _)| Rc::as_ptr(t) as *const () == ptr);
        if !already_tracked {
            frame.deps.push((trackable, keeper));
        }
    });
}

/// Is a selector currently mid-`compute()`? Read sites use this to
/// skip the work of building a keeper (cloning `QueryHandle`, etc.)
/// when no tracking frame is active — the common case for ordinary
/// component reads.
pub(crate) fn currently_tracking() -> bool {
    SELECTOR_STACK.with(|stack| !stack.borrow().is_empty())
}

fn push_frame() {
    SELECTOR_STACK.with(|stack| {
        stack.borrow_mut().push(TrackingFrame { deps: Vec::new() });
    });
}

fn pop_frame() -> TrackingFrame {
    SELECTOR_STACK
        .with(|stack| stack.borrow_mut().pop())
        .expect("pop_frame called without matching push_frame")
}

// ---- Selector entry -----------------------------------------------

/// Object-safe shape over a typed `SelectorEntry<T>`. Lets
/// `QueryClientInner.selectors` hold heterogeneous entries behind one
/// `Weak<dyn AnySelector>`. The `as_rc_any` upcast mirrors the
/// `AnyQuerySubscription::as_rc_any` pattern in `client.rs` and is
/// the safe-stdlib path to `Rc::downcast`.
pub(crate) trait AnySelector: AnyTrackable {
    fn as_rc_any(self: Rc<Self>) -> Rc<dyn Any>;
    /// The args this selector was built with. Used by `observe_selector`
    /// to disambiguate hash collisions in the cache bucket.
    fn args(&self) -> &dyn AnyArgs;
}

/// `(id, callback)` entry in a selector's listener list. The id is
/// what the returned [`TrackToken`] keeps so drop can unregister.
type Listener = (u64, Rc<dyn Fn()>);

/// One memoized selector instance. Keyed in the client by
/// `(SelectorId, args_hash)`.
///
/// Owns the compute closure, the cached output, the set of upstream
/// dependencies it captured on its last run, the listener tokens it
/// holds on each upstream, and its own listener registry.
pub(crate) struct SelectorEntry<T: PartialEq + Clone + 'static> {
    id: SelectorId,
    args_hash: u64,
    /// Owned args (boxed, type-erased) for equality-disambiguation
    /// when multiple entries land in the same hash bucket.
    args: Box<dyn AnyArgs>,
    cached_output: RefCell<Option<T>>,
    /// Erased keepers (cloned `QueryHandle`s for subscriptions,
    /// nested `Rc<SelectorEntry<_>>`s for sub-selectors) holding each
    /// tracked dep alive between reruns.
    keepers: RefCell<Vec<Box<dyn Any>>>,
    /// Listener tokens on each upstream. Drop unregisters.
    listener_tokens: RefCell<Vec<TrackToken>>,
    /// Selector's own listener registry. Fires on diffed output
    /// change after each rerun.
    listeners: RefCell<Vec<Listener>>,
    next_listener_id: Cell<u64>,
    /// Re-entrancy guard. If a rerun's compute() somehow triggers a
    /// notify on a tracked dep that re-enters this selector's
    /// listener callback, the nested rerun is skipped; the outer
    /// rerun's continued execution will observe the updated upstream
    /// state on its next read.
    running: Cell<bool>,
    compute: Box<dyn Fn() -> T>,
    /// Weak back-ref for `Drop` self-removal from the client's
    /// selector registry. Mirrors the `RegistryWeakRef` pattern used
    /// for subscriptions.
    client: Weak<QueryClientInner>,
}

impl<T: PartialEq + Clone + 'static> SelectorEntry<T> {
    pub(crate) fn new(
        id: SelectorId,
        args_hash: u64,
        args: Box<dyn AnyArgs>,
        compute: Box<dyn Fn() -> T>,
        client: Weak<QueryClientInner>,
    ) -> Self {
        Self {
            id,
            args_hash,
            args,
            cached_output: RefCell::new(None),
            keepers: RefCell::new(Vec::new()),
            listener_tokens: RefCell::new(Vec::new()),
            listeners: RefCell::new(Vec::new()),
            next_listener_id: Cell::new(0),
            running: Cell::new(false),
            compute,
            client,
        }
    }

    /// Execute `compute`, capture the tracked deps, diff the output
    /// against the cache, and fire selector listeners on change.
    ///
    /// Steps mirror the explainer's algorithm (see the doc):
    ///   1. Push a tracking frame.
    ///   2. Run `compute()`.
    ///   3. Pop the frame.
    ///   4. Drop old listener tokens; install new keepers + tokens.
    ///   5. If output differs from cache: update cache, fire
    ///      selector's own listeners.
    pub(crate) fn rerun(self: &Rc<Self>) {
        if self.running.get() {
            return;
        }
        self.running.set(true);

        push_frame();
        let new_output = (self.compute)();
        let frame = pop_frame();

        // Re-wire listeners. Drop old tokens FIRST so a same-upstream
        // listener is unregistered before its replacement registers
        // — keeps listener-vec growth bounded across reruns.
        self.listener_tokens.borrow_mut().clear();

        let weak_self: Weak<Self> = Rc::downgrade(self);
        let mut new_keepers: Vec<Box<dyn Any>> = Vec::with_capacity(frame.deps.len());
        let mut new_tokens: Vec<TrackToken> = Vec::with_capacity(frame.deps.len());
        for (trackable, keeper) in frame.deps {
            let w = weak_self.clone();
            let cb: Rc<dyn Fn()> = Rc::new(move || {
                if let Some(this) = w.upgrade() {
                    this.rerun();
                }
            });
            let id = trackable.register_listener(cb);
            new_tokens.push(TrackToken::new(&trackable, id));
            new_keepers.push(keeper);
        }
        *self.keepers.borrow_mut() = new_keepers;
        *self.listener_tokens.borrow_mut() = new_tokens;

        self.running.set(false);

        let changed = {
            let cache = self.cached_output.borrow();
            cache.as_ref() != Some(&new_output)
        };
        if !changed {
            return;
        }
        *self.cached_output.borrow_mut() = Some(new_output);

        // Snapshot listeners before invoking so a callback that
        // registers / drops listeners on this same selector doesn't
        // re-enter the `listeners` RefCell mid-iteration. Mirrors the
        // pattern in `QuerySubscription::notify_listeners`.
        let snapshot: Vec<Rc<dyn Fn()>> = {
            let listeners = self.listeners.borrow();
            listeners.iter().map(|(_, cb)| cb.clone()).collect()
        };
        for cb in snapshot {
            cb();
        }
    }

    /// Clone of the cached output. Callers must ensure the initial
    /// `rerun` has completed before calling (the
    /// `QueryClient::observe_selector` entry point guarantees this).
    pub(crate) fn cached(&self) -> T {
        self.cached_output
            .borrow()
            .clone()
            .expect("selector cache populated by initial rerun before observe returns")
    }
}

impl<T: PartialEq + Clone + 'static> AnyTrackable for SelectorEntry<T> {
    fn register_listener(&self, callback: Rc<dyn Fn()>) -> u64 {
        let id = self.next_listener_id.get();
        self.next_listener_id.set(id.wrapping_add(1));
        self.listeners.borrow_mut().push((id, callback));
        id
    }

    fn unregister_listener(&self, id: u64) {
        if let Ok(mut listeners) = self.listeners.try_borrow_mut() {
            listeners.retain(|(lid, _)| *lid != id);
        }
    }
}

impl<T: PartialEq + Clone + 'static> AnySelector for SelectorEntry<T> {
    fn as_rc_any(self: Rc<Self>) -> Rc<dyn Any> {
        self
    }
    fn args(&self) -> &dyn AnyArgs {
        &*self.args
    }
}

impl<T: PartialEq + Clone + 'static> Drop for SelectorEntry<T> {
    fn drop(&mut self) {
        // Self-remove from the client's selector registry. Walk the
        // bucket at `(id, args_hash)`, prune any dead Weaks. When the
        // bucket empties, drop the slot entirely. Other live entries
        // in the bucket (distinct args that happened to collide on
        // hash) are left alone.
        let Some(inner) = self.client.upgrade() else {
            return;
        };
        let Ok(mut selectors) = inner.selectors.try_borrow_mut() else {
            return;
        };
        let key = (self.id, self.args_hash);
        if let Some(bucket) = selectors.get_mut(&key) {
            bucket.retain(|w| w.strong_count() > 0);
            if bucket.is_empty() {
                selectors.remove(&key);
            }
        }
    }
}

// ---- User-facing view ---------------------------------------------

/// User-facing handle to a cached selector. Returned by the
/// `#[query]` macro's generated `observe()` and by
/// [`QueryClient::observe_selector`](crate::QueryClient::observe_selector).
///
/// Reading [`Self::value`] inside another selector's body records the
/// inner selector as a dependency, so selectors compose recursively.
/// Drop the view to release the selector — entries with no remaining
/// `SelectorView` and no parent selector tracking them are GC'd.
pub struct SelectorView<T: PartialEq + Clone + 'static> {
    entry: Rc<SelectorEntry<T>>,
}

impl<T: PartialEq + Clone + 'static> SelectorView<T> {
    pub(crate) fn from_entry(entry: Rc<SelectorEntry<T>>) -> Self {
        Self { entry }
    }

    /// Current memoized value. Calling this inside another selector's
    /// body records this selector as a dep of the caller — that's
    /// how nested selectors compose. Outside any selector, the call
    /// is a plain cache read.
    pub fn value(&self) -> T {
        let trackable: Rc<dyn AnyTrackable> = self.entry.clone();
        record_read(trackable, Box::new(self.entry.clone()));
        self.entry.cached()
    }

    /// Register a callback to fire whenever the selector's diffed
    /// output changes. Returned token unregisters on drop.
    pub fn on_update<F: Fn() + 'static>(&self, callback: F) -> TrackToken {
        let cb: Rc<dyn Fn()> = Rc::new(callback);
        let trackable: Rc<dyn AnyTrackable> = self.entry.clone();
        let id = trackable.register_listener(cb);
        TrackToken::new(&trackable, id)
    }
}
