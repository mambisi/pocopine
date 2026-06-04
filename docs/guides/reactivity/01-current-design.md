---
title: "Reactivity internals"
description: "How pocopine's reactive runtime tracks dependencies, schedules updates, and exposes computed values and stores."
---

# Reactivity internals

## Thread-locals

Everything lives in per-wasm-module `thread_local`s. WASM is single-threaded,
so these are effectively module-globals with safe access from any call site.

| Thread-local | Type | Role |
|---|---|---|
| `NEXT_ID` | `Cell<u64>` | Monotonic id source for scopes and effects. Starts at `1`; `0` is reserved as a sentinel. |
| `CURRENT_EFFECT` | `Cell<Option<EffectId>>` | The effect running right now. Read by `track`. |
| `EFFECTS` | `HashMap<EffectId, Rc<dyn Fn()>>` | The effect body. Rerun by id during flush. |
| `SCHEDULERS` | `HashMap<EffectId, Rc<dyn Fn(EffectId)>>` | Per-effect custom schedulers. When present, `trigger` calls the scheduler inline instead of queueing the effect. Used by `computed` to flip its dirty bit. |
| `DEPS` | `HashMap<ScopeId, HashMap<Key, HashSet<EffectId>>>` | Forward map: given a scope and field name, which effects to rerun. Two-level nesting keeps `trigger_scope` O(k) in the scope's key count. |
| `REVERSE` | `HashMap<EffectId, HashSet<(ScopeId, Key)>>` | Back map: given an effect, which `(scope, key)` pairs it has subscribed to. Used by `clear_deps_for` to remove stale subscriptions before each rerun. |
| `QUEUE` | `HashSet<EffectId>` | Effects pending rerun. Drained by `flush`. |
| `FLUSH_SCHEDULED` | `Cell<bool>` | Guards against scheduling more than one flush microtask at a time. |
| `CLEANUPS` | `HashMap<EffectId, Vec<Box<dyn FnOnce()>>>` | Teardown hooks registered via `on_cleanup`. Run before the next rerun or when `release` is called. |
| `BATCHING` | `Cell<u32>` | Nestable batch counter. Flush is deferred until the outermost `batch` call completes. |
| `AUTO_FLUSH` | `Cell<bool>` | Disabling this (tests only) holds queued effects until `flush_sync` is called explicitly. |
| `TRIGGER_SCRATCH` | `RefCell<Vec<EffectId>>` | Reusable per-thread snapshot buffer so `dispatch_subs` does not clone a `HashSet` on every trigger. |

A second, id-keyed dep table (`SIGNAL_DEPS` / `SIGNAL_REVERSE` in
`reactive.rs`) keys subscriptions on a numeric id directly instead of a
`(ScopeId, key)` pair — no string allocation per access. `computed` rides
on it for its own dirty-notification edges. It shares the same effect
engine, queue, flush, and batching as the proxy-scoped table.

## The lifecycle of a read

```text
directive body runs inside effect(f)
  └─ CURRENT_EFFECT = Some(id)
     └─ Reflect::get(&proxy, "count")
        └─ proxy.get trap fires (in JS, calling back to Rust)
           └─ track(scope_id, "count")
              ├─ DEPS[scope_id]["count"].insert(current_effect)
              └─ REVERSE[current_effect].insert((scope_id, "count"))
           └─ return FIELD_CACHE[scope_id]["count"]
              // or state.borrow().get("count") on a cache miss
```

Field values are cached in `FIELD_CACHE` after the first serialisation.
Subsequent reads within the same mutation cycle return the cached `JsValue`
without going through `serde_wasm_bindgen` again. The cache is invalidated
per-field by the proxy `set` trap and per-scope after a handler invocation.

## The lifecycle of a write

Two paths for proxy-backed component fields, both converge at `dispatch_subs`.

**Through the proxy** — a `pp-model` input event or a direct assignment in
a template expression:

```text
Reflect::set(&proxy, "count", 3)
  └─ proxy.set trap
     ├─ state.borrow_mut().set("count", 3)
     ├─ FIELD_CACHE[scope_id].remove("count")
     └─ trigger(scope_id, "count")
        └─ dispatch_subs(DEPS[scope_id]["count"])
           ├─ effects with a custom scheduler: scheduler(effect_id) called inline
           └─ remaining effects: QUEUE.insert(effect_id)
        └─ schedule_flush()
```

**Through a handler** — `#[handlers] fn increment(&mut self) { self.count += 1; }`
mutates Rust state directly, bypassing the proxy. The runtime cannot know
which fields changed inside a plain `&mut self` method, so `Scope::invoke`
calls `trigger_scope(id)` after the handler returns. This fans out to every
currently-tracked key of that scope — coarser than a single-field `trigger`,
but correct. The `FIELD_CACHE` for the scope is also invalidated at this
point (except for fields explicitly kept fresh by `patch_*_inline` helpers).

## Flushing

`schedule_flush` spawns a microtask via
`wasm_bindgen_futures::spawn_local`, which awaits
`JsFuture::from(Promise::resolve(&JsValue::NULL))`. When the resolved
promise settles, `flush` drains the queue and reruns each pending effect.

Re-running an effect:

1. Run all `on_cleanup` hooks registered during the previous run.
2. `clear_deps_for(id)` — removes this effect from both the proxy
   (`DEPS`/`REVERSE`) and id-keyed (`SIGNAL_DEPS`/`SIGNAL_REVERSE`) tables.
3. Set `CURRENT_EFFECT = Some(id)`, run the body, restore the previous value.

The clear-before-run step keeps conditional reads correct. If the body ran
`if a { b } else { c }` and `a` flips, the stale subscription on `b` is
dropped before the new dep set around `c` is built.

Effects that queue themselves during a flush land in the **next** batch,
because `QUEUE.drain()` snapshots the queue before any effect body runs.

## Batching

`batch(f)` coalesces multiple writes into a single flush:

```rust
let cart = store::<Cart>();
batch(|| {
    cart.update(|c| c.add(item_a));
    cart.update(|c| c.add(item_b));
});
// one microtask flush, not two
```

`batch` is nestable — the flush is deferred until the outermost call
completes and then scheduled only if the queue is non-empty.

## Computed values

`#[computed]` methods compile to the `computed(f)` primitive: a memoised
derivation over any reactive sources.

```rust
#[handlers]
impl FullName {
    #[computed]
    pub fn full(first: String, last: String) -> String {
        format!("{first} {last}")
    }
}
```

`computed` is lazy — the derivation runs on the first read and only
re-evaluates when a dep has changed **and** something reads the result
again. Internally it is a lazy effect with a custom scheduler that flips a
`dirty` bit and re-notifies the computed's own subscribers through the
id-keyed table. Dropping the `Computed<T>` releases the underlying effect.

## Effect cleanup

`on_cleanup` registers a teardown hook that runs before the next rerun or
when `release` is called:

```rust
effect_scoped(|| {
    let handle = timers::every(1000, || tracing::info!(target: "pocopine.log", "tick"));
    on_cleanup(move || handle.cancel());
});
```

## Stores

`#[store]` scopes outlive any particular DOM mount. One instance per type
per runtime:

```rust
#[store]
pub struct Preferences {
    pub theme: String,
}
```

In templates, `$store.preferences.theme` resolves through the store's proxy
and participates in normal dep tracking. In Rust, `store::<Preferences>()`
returns a `Handle<Preferences>` (the same type `this::<T>()` returns for a
component), exposing `update` and `with` closures over the concrete state.

## Current constraints

- **Reactivity is per field, by name.** `"count"` is a string key matched
  against the component's declared fields. There is no nested field
  tracking, no array element tracking, and no index tracking in
  collections.
- **Handler mutations trigger every key in scope.** Fine for small
  components; for a component with many cold fields and one hot one,
  prefer a `pp-model`-driven proxy assignment so the write goes through
  the single-field `trigger` path instead of the blanket `trigger_scope`.
- **`trigger_scope` is O(k)**, not O(n), thanks to the nested `DEPS` map,
  but it still fans out to all currently tracked keys. A handler that
  touches one field out of twenty will still rerun all twenty effects that
  tracked any field in the scope.
- **Scheduler is single-tier.** Flush runs all queued effects in an
  unordered batch. There are no pre/post/idle priority groups.
