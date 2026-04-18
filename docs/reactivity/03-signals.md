# Signals — design sketch

First feature out of the "deeper reactivity" slice. This is a proposal, not
committed code. Lives alongside the existing proxy/field model rather than
replacing it.

## Shape

```rust
use pocopine::reactive::{signal, computed, watch};

let (count, set_count) = signal(0);           // (Signal<i32>, Setter<i32>)
let doubled = computed(move || count.get() * 2);

effect(move || {
    console::log_1(&format!("{} / {}", count.get(), doubled.get()).into());
});

set_count.set(1);   // effect reruns with 1 / 2
set_count.update(|n| *n += 1); // -> 2 / 4
```

Why two halves (`Signal` / `Setter`)? Aliasing. A handler that only reads
should be unable to mutate. Same split Solid uses. For the "read and write"
case we provide `let count = rw_signal(0)` returning a combined handle.

## Types

```rust
pub struct Signal<T>     { id: SignalId, cell: Rc<RefCell<T>> }
pub struct Setter<T>     { id: SignalId, cell: Rc<RefCell<T>> }
pub struct RwSignal<T>   { id: SignalId, cell: Rc<RefCell<T>> }
pub struct Computed<T>   { /* see below */ }

// Read / mutate API
impl<T: Clone> Signal<T>  { pub fn get(&self) -> T }
impl<T>       Signal<T>  { pub fn with<R>(&self, f: impl FnOnce(&T) -> R) -> R }
impl<T>       Setter<T>  { pub fn set(&self, v: T); pub fn update(&self, f: impl FnOnce(&mut T)) }
```

`SignalId` is a newtype around `u64`. It integrates with the existing
effect engine via a **synthetic scope** — reactive.rs gains a constant
`SIGNAL_SCOPE: ScopeId = ScopeId(0)` and every `SignalId` maps to the
`(SIGNAL_SCOPE, signal_id_string)` key. No new dep-map; no new flush path.

```rust
// Inside Signal::get
fn get(&self) -> T where T: Clone {
    reactive::track(SIGNAL_SCOPE, &self.id.to_string());
    self.cell.borrow().clone()
}
// Inside Setter::set
fn set(&self, v: T) {
    *self.cell.borrow_mut() = v;
    reactive::trigger(SIGNAL_SCOPE, &self.id.to_string());
}
```

(Stringifying an ID per call is lazy — if hot enough to matter, the
dep-map key becomes a `Cow<'static, str>` or an enum; but measure first.)

## `computed`

```rust
pub struct Computed<T: Clone + 'static> {
    id: SignalId,
    cell: Rc<RefCell<Option<T>>>,  // None = dirty
    _effect: EffectId,
}

pub fn computed<T: Clone + 'static>(f: impl Fn() -> T + 'static) -> Computed<T> {
    let id = next_signal_id();
    let cell = Rc::new(RefCell::new(None));
    let cell_w = cell.clone();
    let eff = effect(move || {
        let v = f();
        *cell_w.borrow_mut() = Some(v);
        reactive::trigger(SIGNAL_SCOPE, &id.to_string());
    });
    Computed { id, cell, _effect: eff }
}

impl<T: Clone> Computed<T> {
    pub fn get(&self) -> T {
        reactive::track(SIGNAL_SCOPE, &self.id.to_string());
        self.cell.borrow().clone().expect("computed never ran")
    }
}
```

The effect engine already handles dep cleanup on rerun, so the computed's
source subscriptions rebuild automatically each time its body reruns. The
key insight: *a computed is just an effect whose body writes into a cell
and triggers its own signal*. No new machinery.

## `watch`

```rust
pub fn watch<T: Clone + PartialEq + 'static>(
    source: impl Fn() -> T + 'static,
    cb: impl Fn(&T, Option<&T>) + 'static,
) -> EffectId {
    let prev: Rc<RefCell<Option<T>>> = Rc::new(RefCell::new(None));
    let prev_w = prev.clone();
    effect(move || {
        let next = source();
        let last = prev_w.borrow().clone();
        if last.as_ref() != Some(&next) {
            cb(&next, last.as_ref());
        }
        *prev_w.borrow_mut() = Some(next);
    })
}
```

Returns the `EffectId` so callers can `release` it. If we want a
friendlier handle we wrap it — probably later.

## Cleanup hooks

```rust
thread_local! {
    static CLEANUPS: RefCell<HashMap<EffectId, Vec<Box<dyn FnOnce()>>>> = ...;
}

pub fn on_cleanup(f: impl FnOnce() + 'static) {
    let Some(id) = reactive::current_effect() else { return };
    CLEANUPS.with(|c| c.borrow_mut().entry(id).or_default().push(Box::new(f)));
}
```

Two places call `run_cleanups(id)`:
1. Top of `run_effect` (before clearing deps), so each rerun tears down
   the previous run's resources.
2. Inside `release`, for the final teardown.

Typical use:

```rust
effect(|| {
    let handle = set_interval(..., 1000);
    on_cleanup(move || clear_interval(handle));
});
```

## Batching

```rust
pub fn batch<R>(f: impl FnOnce() -> R) -> R {
    BATCHING.with(|b| b.set(b.get() + 1));
    let r = f();
    BATCHING.with(|b| b.set(b.get() - 1));
    if BATCHING.with(|b| b.get()) == 0 {
        schedule_flush(); // already deduped via FLUSH_SCHEDULED
    }
    r
}
```

`trigger` checks `BATCHING` — if nonzero, push to queue but skip the
flush schedule. Fewest lines possible; just defers the moment we call
`schedule_flush`.

## Why this order

Signals first, because they unlock computed (almost free once signals
exist), watch (three lines), on_cleanup (independent but tiny), and
batch (ditto). Each later feature becomes a 20–30 line PR on top of the
last. No feature on this page forces a rewrite of anything in
`reactive.rs` — we only add one synthetic scope.

## What this explicitly does *not* solve

- Fine-grained triggers from handlers (still coarse `trigger_scope`).
- Nested object/array reactivity (still bound to flat `(scope, key)`).
- Stores. Stores probably get built *on top of* signals once we like the
  shape of this API.
