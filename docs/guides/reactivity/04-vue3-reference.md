---
title: "Vue 3 as a reference"
description: "How pocopine's reactive engine maps to the Vue 3 deep-dive primitives — where the designs converge, where they diverge, and what remains on the roadmap."
---

# Vue 3 as a reference

The [vue3-deep-dive](https://github.com/pinglu85/vue3-deep-dive) reference repo — specifically the
`7-intro-to-reactivity/`, `8-building-reactivity-from-scratch/deps.html`, and
`9-building-the-reactive-api/reactive.html` chapters — walks through the same
dependency-tracking primitives that power pocopine's reactive core. This page
maps each concept to its current implementation.

## Where Vue 3 and pocopine agree

| Concept | Vue 3 | pocopine |
|---|---|---|
| "currently running effect" | global `activeEffect` | `CURRENT_EFFECT: Cell<Option<EffectId>>` |
| proxy trap on read | `get(target, key)` → `dep.depend()` | `get` closure → `track(scope_id, key)` |
| proxy trap on write | `set(target, key, val)` → `dep.notify()` | `set` closure → `trigger(scope_id, key)` |
| "the subscribers of a (target, key)" | `WeakMap<obj, Map<key, Set<Effect>>>` | `HashMap<ScopeId, HashMap<Key, HashSet<EffectId>>>` |
| effect rerun | `subscribers.forEach(e => e())` | drain `QUEUE` in microtask |
| boxed single value | `ref(value)` → `.value` get/set | `signal(value)` → `Signal::get` / `Setter::set` (advanced) |

The shape is the same. pocopine keys by `(ScopeId, key)` instead of `(raw_object, key)` because
Rust cannot hash a `JsValue` by identity, and each component scope already has a stable numeric id.

The dependency table uses a two-level nested map — `DEPS[scope_id][key]` — rather than a flat
`HashMap<(ScopeId, Key), _>`. The two-level layout lets `HashMap` resolve a `&str` lookup directly
(via `Cow<'static, str>: Borrow<str>`) without allocating a compound key, and makes
`trigger_scope` O(k) in the scope's live keys rather than O(|DEPS|).

A second, id-keyed table (`SIGNAL_DEPS` / `SIGNAL_REVERSE`) keys subscriptions on a numeric id
directly — no per-access string conversion. `computed` rides on it for its dirty-notification
edges. Both tables share the same effect engine, queue, flush, and batching machinery.

## reactive() by default, ref() as an escape hatch

Vue 3 ships two reactive primitives: `reactive(obj)` wraps an object in a proxy, and `ref(value)`
boxes a single value in a `.value` cell. pocopine leads with **the proxy** and treats the boxed
cell as a rarely-needed advanced tool.

A `#[component]` *is* a `reactive()` object. Its fields are tracked through the scope proxy exactly
like properties of a Vue `reactive` target — there is no `.value`, no read/write split, no cell to
construct. This is the **default mental model**: a Rust struct field is already a typed, named,
tool-visible value, so for component state you never wrap anything.

```rust
// Vue: const state = reactive({ count: 0 }); state.count++
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub count: i32,
}

#[handlers]
impl Counter {
    pub fn increment(&mut self) { self.count += 1; }
}
```

pocopine **does** ship a `ref()` analog — `signal()` / `rw_signal()`. `signal(initial)` returns a
`(Signal<T>, Setter<T>)` read/write pair; `rw_signal(initial)` returns a combined `RwSignal<T>`.
`Signal::get` subscribes the current effect and `Setter::set` notifies subscribers — the same
read-tracks / write-triggers contract as Vue's `ref.value`, just split across two handles instead
of a single `.value` getter/setter.

```rust
use pocopine::prelude::{signal, rw_signal};

// Vue: const count = ref(0); count.value++
let (count, set_count) = signal(0);
let _ = count.get();        // subscribe (Vue: read count.value)
set_count.set(1);           // notify   (Vue: count.value = 1)

// Combined handle when ergonomics win:
let count = rw_signal(0);
count.set(count.get() + 1);
```

Unlike Vue, this is **not** the everyday path. Signals are a deliberately **rare escape hatch** for
standalone reactive state that is *not* owned by a component — library code, a module-level reactive
value, a bridge over an external source. For ordinary application state prefer proxy-scoped struct
fields, or `#[store]` for the app-wide case. See
[Utilities — Advanced: standalone signals](./02-utilities.md#advanced-standalone-signals) for when
the escape hatch is the right call.

Derived values that Vue would write as `computed(() => ...)` are `#[computed]` methods; reactions
Vue would write as `watch(...)` / `watchEffect(...)` are `#[watch(field)]` methods or the
`watch` / `effect` free functions.

## Where Vue 3 maps to shipped features

### `computed()` — lazy memoized derivation

Vue's computed uses a `dirty` flag and a custom `scheduler`. pocopine's `computed` (driven by the
`#[computed]` attribute) follows the same pattern exactly:

- The backing effect is registered with `lazy: true` so it does not run at construction time.
- The scheduler sets `dirty = true` and re-notifies downstream effects through the computed's own
  id — it does not recompute until a caller reads.
- The first read reruns the effect via `run_now`, then tracks via the computed's own id.

```rust
#[handlers]
impl Cart {
    #[computed]
    pub fn doubled(count: i32) -> i32 {
        count * 2
    }
}
```

`Computed` implements `Drop` and calls `release(effect_id)` automatically, so its internal effect
is cleaned up when the handle goes out of scope.

### `effect(fn, { scheduler })` — effects with custom scheduling

`effect_with` is the low-level primitive that `computed` and test-mode flush both build on:

```rust
use std::rc::Rc;
use pocopine_core::reactive::{effect_with, EffectId, EffectOptions};

let id = effect_with(
    move || { /* body */ },
    EffectOptions {
        lazy: false,
        scheduler: Some(Rc::new(|_eid: EffectId| {
            // called by trigger instead of the default queue+flush path
        })),
    },
);
```

`EffectOptions::lazy` — if `true`, the effect is stored but not run immediately.

`EffectOptions::scheduler` — if `Some`, `trigger` hands control to this closure with the
`EffectId` instead of pushing to `QUEUE`. The default (no scheduler) pushes to `QUEUE` and
schedules a microtask flush via a resolved `Promise`.

Test-mode flush (`set_auto_flush(false)` + `flush_sync()`) uses the same engine: the auto-flush
path is gated behind `AUTO_FLUSH`, so callers that need deterministic control disable it and drive
`flush_sync` themselves without spinning the JS event loop.

## Where Vue 3 does more — roadmap items

### Deep / shallow / readonly

Vue's `reactive(obj)` wraps recursively — reading `user.address.street` returns a proxy of
`address` that tracks `.street`. `shallowReactive` stops at the first level; `readonly` forbids
`set`.

pocopine's proxy scope is currently flat: a component is a single struct with primitive or
owned fields, so there is no recursive proxy chain. Per-item reactivity on a `Vec<TodoItem>`
field — and the `pp-for` directive that needs it — is the gate for this work.

### Collection handlers

Vue ships separate proxy handler sets for `Map`, `Set`, `WeakMap`, and `WeakSet` because their
mutating methods bypass normal property traps. The key shape: reads track, writes trigger, and
any insert or delete also triggers a synthetic "iteration" key so observers iterating the
collection are notified.

pocopine's equivalent would build tracking around macro-generated accessors on `Vec<T>` /
`HashMap<K, V>` fields rather than JS proxy traps, but the iteration-key concept carries over
directly. This is gated behind deep reactive support.

### `effectScope()`

Vue groups effects under a scope handle so `scope.stop()` releases them all at once. pocopine
already tracks per-element effects and releases them on unmount (via `mount::release_subtree`);
exposing a user-facing `EffectScope` handle for manually-grouped teardown is the remaining step.

### Op-typed triggers (`TriggerOpTypes`)

Vue's `trigger` carries an operation kind — `SET`, `ADD`, `DELETE`, `CLEAR`. A plain `SET` does
not re-notify effects that are subscribed to the collection's iteration key, but `ADD` and `DELETE`
do. pocopine does not need this for flat scalar fields; it becomes necessary once reactive
collections land.

### Flush timing tiers

Vue's job queue distinguishes pre-flush, sync, and post-flush callbacks (`queueJob` /
`queuePostFlushCb`). pocopine's flush currently runs all queued effects in a single microtask.
Timing tiers become relevant once transitions or async components require post-flush DOM reads.

## Feature status

| Feature | Status |
|---|---|
| Proxy-scoped fields (`reactive`) | Shipped (default) |
| `signal` / `rw_signal` (`ref`) | Shipped (advanced escape hatch) |
| `#[computed]` (lazy + scheduler) | Shipped |
| `#[watch(field)]` / `watch` / `watch_field` | Shipped |
| `effect_with(opts)` — lazy + custom scheduler | Shipped |
| `batch` | Shipped |
| `on_cleanup` | Shipped |
| `#[store]` (app-wide reactive singleton) | Shipped |
| Deep / shallow reactive | Roadmap (gates `pp-for` per-item) |
| Collection handlers | Roadmap (same gate) |
| `effectScope` (user-facing) | Roadmap |
| Op-typed triggers | Roadmap (after deep reactive) |
| Flush timing tiers | Roadmap (after transitions / async) |

## Reading path

Start with `9-building-the-reactive-api/reactive.html` in the reference repo — it is the
condensed build. Back-fill with `8-building-reactivity-from-scratch/deps.html` if any step
is opaque.

Then map the concepts onto the live API:

- [Overview](./README.md) — the mental model and the "what to reach for" routing table.
- [Essentials](./01-essentials.md) — the everyday `#[component]` / `#[computed]` / `#[watch]` / `#[store]` cookbook.
- [Utilities](./02-utilities.md) — the free-fn toolbox, including [Advanced: standalone signals](./02-utilities.md#advanced-standalone-signals) (the `ref()` escape hatch).
- [Internals](./03-internals.md) — the proxy `(ScopeId, key)` table and the id-keyed signal table `computed` rides on.
- [Roadmap](./05-roadmap.md) — what is shipped vs. pending of the items above.
