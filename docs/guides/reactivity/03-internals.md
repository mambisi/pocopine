---
title: "Reactivity internals"
description: "How pocopine's reactive runtime tracks dependencies, schedules updates, and exposes computed values and stores — for people extending the engine."
---

# Reactivity internals

This is the engine room. For the everyday API see
[Essentials](./01-essentials.md) and [Utilities](./02-utilities.md); for the
overview and routing table start at the [Overview](./README.md). This page
is for extenders who need to know how the runtime actually works.

## Big picture

Pocopine has no virtual DOM and no render pass. A directive is just an
**effect** — a closure registered with the reactive engine. When that
closure runs it reads component fields through a `js_sys::Proxy`; each read
**subscribes** the running effect to a `(scope, field)` pair. When the field
is written, the engine **requeues** every subscribed effect and reruns them
on the next microtask. There is no diffing — the effect re-reads the value
and patches its own piece of the DOM.

```text
read  →  proxy get trap  →  track(scope, key)   subscribe running effect
write →  proxy set trap  →  trigger(scope, key) →  queue subscribers
                                                  →  microtask flush → rerun
```

Three moving parts hold this together, and the rest of this page walks each:

- **Two dependency tables.** A proxy table keyed on `(ScopeId, key)` for
  component fields, and a separate id-keyed table for signals — which
  `computed` rides on. Both feed the same effect engine, queue, flush, and
  batch counter.
- **An effect engine.** Effects, their schedulers, cleanups, and reverse
  dep edges, all keyed on `EffectId`.
- **A microtask flush.** A single coalesced drain of the queue, scheduled
  via a resolved-promise microtask.

Everything below lives in `crates/pocopine-core/src/reactive.rs` (the engine)
and `scope.rs` (the proxy traps and handler dispatch), with `computed.rs`,
`signal.rs`, and `store.rs` as consumers.

## Thread-locals

Everything lives in per-wasm-module `thread_local`s. WASM is single-threaded,
so these are effectively module-globals with safe access from any call site.

| Thread-local | Type | Role |
|---|---|---|
| `NEXT_ID` | `Cell<u64>` | Monotonic id source shared by scopes, effects, and signals. Starts at `1`; `ScopeId(0)` is reserved as the `SIGNAL_SCOPE` sentinel. |
| `CURRENT_EFFECT` | `Cell<Option<EffectId>>` | The effect running right now. Read by `track` / `track_signal` to know who is subscribing. |
| `EFFECTS` | `HashMap<EffectId, Rc<dyn Fn()>>` | The effect body. Rerun by id during flush. |
| `SCHEDULERS` | `HashMap<EffectId, Rc<dyn Fn(EffectId)>>` | Per-effect custom schedulers. When present, dispatch calls the scheduler inline instead of queueing. Used by `computed` to flip its dirty bit. |
| `DEPS` | `HashMap<ScopeId, HashMap<Key, HashSet<EffectId>>>` | Proxy forward map: given a scope and field name, which effects to rerun. Two-level nesting keeps `trigger_scope` O(k) in the scope's live key count. |
| `REVERSE` | `HashMap<EffectId, HashSet<(ScopeId, Key)>>` | Proxy back map: given an effect, which `(scope, key)` pairs it subscribed to. Used by `clear_deps_for` to drop stale subscriptions before each rerun. |
| `SIGNAL_DEPS` | `HashMap<SignalId, HashSet<EffectId>>` | Signal forward map, keyed on `SignalId` directly (see [the dual dep tables](#the-dual-dependency-tables)). |
| `SIGNAL_REVERSE` | `HashMap<EffectId, HashSet<SignalId>>` | Signal back map; the signal-table counterpart to `REVERSE`. |
| `QUEUE` | `HashSet<EffectId>` | Effects pending rerun. Drained by `flush`. |
| `FLUSH_SCHEDULED` | `Cell<bool>` | Guards against scheduling more than one flush microtask at a time. |
| `CLEANUPS` | `HashMap<EffectId, Vec<Box<dyn FnOnce()>>>` | Teardown hooks registered via `on_cleanup`. Run before the next rerun or when `release` is called. |
| `BATCHING` | `Cell<u32>` | Nestable batch counter. Flush is deferred until the outermost `batch` completes. |
| `AUTO_FLUSH` | `Cell<bool>` | Disabling this (tests only) holds queued effects until `flush_sync` runs. |
| `TRIGGER_SCRATCH` | `RefCell<Vec<EffectId>>` | Reusable per-thread snapshot buffer so dispatch does not clone a `HashSet` on every trigger. |

The proxy `Key` type is `Cow<'static, str>`: a macro-generated `&'static str`
threads through to the `HashMap` without allocation, while a dynamically built
key (proxy traps, dotted paths) owns its string exactly once. `HashMap`'s
`Borrow` support lets lookups probe with a bare `&str`.

## The dual dependency tables

There are **two** dependency tables, sharing one effect engine but keyed
differently.

**The proxy table** (`DEPS` / `REVERSE`) keys subscriptions on
`(ScopeId, key)` — the scope plus the field name. This is what component
fields and stores ride on. The nesting (`DEPS[scope][key]`) is deliberate:

1. Key lookup uses `&str` directly via `Cow<'static, str>: Borrow<str>`. A
   flat `HashMap<(ScopeId, Key), _>` would need a `(ScopeId, Cow)` probe,
   forcing a string allocation on every lookup.
2. `trigger_scope` becomes O(k) in the scope's live keys — iterate the inner
   map — instead of scanning every `(scope, key)` pair in the app.

**The id-keyed signal table** (`SIGNAL_DEPS` / `SIGNAL_REVERSE`) keys
subscriptions on a numeric `SignalId` directly — no scope, no string. Signals
used to piggyback on the proxy map via a stringified id plus the
`SIGNAL_SCOPE` pseudo-scope (`ScopeId(0)`), which cost two allocations per
access — one in `id.to_string()`, one in the key's `.to_owned()`. The
dedicated table makes that cost zero. The `SIGNAL_SCOPE` sentinel is still
reserved so a future path could re-join the two tables without a scope-id
collision.

Both `signal`/`rw_signal` **and** `computed` ride the id-keyed table:

- `signal::get` / `RwSignal::get` call `track_signal(id)`; `Setter::set` /
  `update` call `trigger_signal(id)`.
- `computed` allocates a `SignalId` for its *own* result and uses
  `trigger_signal` to notify downstream readers when it goes dirty (see
  [Computed internals](#computed-internals)).

The two tables converge at `dispatch_subs`: `trigger` looks subscribers up in
`DEPS`, `trigger_signal` looks them up in `SIGNAL_DEPS`, and both hand the
resulting `HashSet<EffectId>` to the same dispatch path. An effect that reads
both a proxy field and a signal ends up with edges in `REVERSE` *and*
`SIGNAL_REVERSE`; `clear_deps_for` clears both before every rerun.

## The lifecycle of a read

```text
directive body runs inside effect(f)
  └─ CURRENT_EFFECT = Some(id)
     └─ Reflect::get(&proxy, "count")
        └─ proxy get trap fires (in JS, calling back to Rust)
           └─ track(scope_id, "count")
              ├─ DEPS[scope_id]["count"].insert(current_effect)
              └─ REVERSE[current_effect].insert((scope_id, "count"))
           └─ return FIELD_CACHE[scope_id]["count"]
              // or state.borrow().get("count") on a cache miss
```

`track` short-circuits when the subscription already exists — the common
hot path of an effect re-reading fields it is already subscribed to bails
without any allocation. Only on the first track does it own the string once
and share the `Cow::Owned` across the `DEPS` key and the `REVERSE` entry.

Field values are cached in `FIELD_CACHE` (in `scope.rs`) after the first
serialisation. Subsequent reads of the same field within a trigger cycle
reuse the cached `JsValue` without going through `serde_wasm_bindgen` again.
The cache is invalidated per-field by the proxy set trap and per-scope after
a handler invocation. Derived scopes (`SlotScope` and friends) declare
themselves non-cacheable — they recompose their return value from a parent
proxy on every read, so caching would freeze the value.

## The lifecycle of a write

Two paths for proxy-backed component fields, both converging at
`dispatch_subs`.

**Through the proxy** — a `pp-model` input event or a direct assignment in a
template expression:

```text
Reflect::set(&proxy, "count", 3)
  └─ proxy set trap
     ├─ state.borrow_mut().set("count", 3)
     ├─ FIELD_CACHE[scope_id].remove("count")
     └─ trigger(scope_id, "count")
        └─ dispatch_subs(DEPS[scope_id]["count"])
           ├─ effects with a custom scheduler: scheduler(effect_id) inline
           └─ remaining effects: QUEUE.insert(effect_id)
        └─ schedule_flush()
```

When the field is a `flatten` leaf, the set trap also resolves the container
key, invalidates its cache, and triggers it too — so `#[watch(<container>)]`
fires alongside the per-leaf watch.

**Through a handler** — `#[handlers] fn increment(&mut self) { self.count += 1; }`
mutates Rust state directly, bypassing the proxy. The runtime cannot know
which fields a plain `&mut self` method changed, so `Scope::invoke` calls
`invalidate_field_cache(id)` then `trigger_scope(id)` after the handler
returns. `trigger_scope` clones the scope's live key list (so effect reruns
can mutate `DEPS` mid-flush) and calls `trigger` for each — fanning out to
every tracked key of that scope. Coarser than a single-field `trigger`, but
correct. Targeted op APIs (`patch_*_inline`) keep the cache valid and fire
only the named field instead of dropping the whole cache.

`Scope::invoke` also binds `CURRENT_SCOPE_ID` for the duration of the call so
`this::<T>()` and `dispatch!` resolve to the right component; `Handle::update`
takes the same blanket-`trigger_scope` path.

## Dispatch and flushing

`dispatch_subs` is the shared tail of both `trigger` and `trigger_signal`. It
snapshots the subscriber set into the `TRIGGER_SCRATCH` buffer (taking
ownership of it, so a re-entrant dispatch from an inline scheduler doesn't
clobber the outer call's snapshot), then for each subscriber: if it has a
custom scheduler, call it inline; otherwise push it onto `QUEUE`. If anything
was queued and no batch is open, it calls `schedule_flush`.

`schedule_flush` spawns a microtask via `wasm_bindgen_futures::spawn_local`,
awaiting `JsFuture::from(Promise::resolve(&JsValue::NULL))`. When the resolved
promise settles, `flush` drains the queue and reruns each pending effect.
`FLUSH_SCHEDULED` ensures at most one such microtask is in flight at a time.

Re-running an effect (`run_effect`):

1. Run all `on_cleanup` hooks registered during the previous run — a cleanup
   registered on iteration N belongs to N, not N+1.
2. `clear_deps_for(id)` — removes this effect from **both** the proxy
   (`DEPS`/`REVERSE`) and the id-keyed (`SIGNAL_DEPS`/`SIGNAL_REVERSE`)
   tables.
3. Set `CURRENT_EFFECT = Some(id)`, run the body, restore the previous value.

The clear-before-run step keeps conditional reads correct. If the body ran
`if a { b } else { c }` and `a` flips, the stale subscription on `b` is
dropped before the new dep set around `c` is built.

`flush` snapshots and clears the queue before running any body
(`QUEUE.drain()`), so effects that re-trigger during their run land in the
**next** batch, not the current one.

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

It increments `BATCHING` on entry and decrements on exit; while the counter
is non-zero, `dispatch_subs` queues subscribers but skips `schedule_flush`.
`batch` is nestable — only the outermost call drops the counter to zero, and
it then schedules the deferred flush solely if the queue is non-empty.

## Computed internals

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

`computed` is lazy. Internally it is a **lazy effect with a custom scheduler**
plus a `Rc<RefCell<{ cached, dirty }>>` cell and its own `SignalId`:

- The effect body evaluates `f`, stores the result in `cached`, and clears
  `dirty`. It is registered with `lazy: true`, so it does not run at
  construction.
- The scheduler (invoked inline by `dispatch_subs` when a source changes,
  instead of queueing) sets `dirty = true` and calls `trigger_signal(id)` on
  the computed's own signal — notifying *its* subscribers.
- `Computed::get` calls `run_now(effect_id)` only if `dirty`, then
  `track_signal(id)` to subscribe the reader, then returns a clone of
  `cached`.

So sources are re-evaluated only when a dep has changed **and** something
reads the result again. The dirty-notification edge is exactly why `computed`
needs the id-keyed signal table. Dropping the `Computed<T>` releases the
underlying effect via `release`, so parent effect scopes stay tidy without a
manual teardown step.

## Signals internals

A signal is a `(Signal<T>, Setter<T>)` pair (or a combined `RwSignal<T>`)
sharing one `Rc<RefCell<T>>` and one `SignalId`. `get` / `with` call
`track_signal`; `set` / `update` call `trigger_signal`. `Setter::set` and
`RwSignal::set` carry a `PartialEq` value-equality guard — a write equal to
the current value is a no-op, which prevents a class of watcher-thrash and
render-loop bugs where an effect writes back a value it just read. The
`set_force` / `update` variants fire unconditionally.

Signals are the [standalone escape hatch](./02-utilities.md#advanced-standalone-signals),
not the everyday model — but they exercise the same engine, so understanding
the id-keyed table covers both signals and `computed`.

## Stores internals

`#[store]` scopes outlive any particular DOM mount — one instance per type
per runtime:

```rust
#[store]
pub struct Preferences {
    pub theme: String,
}
```

The macro implements the `Store` trait (`STORE_NAME`, an idempotent
`__register_store`, and `__handle`). A store's `Scope` registers in the
name-keyed `STORE_SCOPES` table under its kebab-cased ident. In templates,
`$store.preferences.theme` resolves through that scope's proxy and
participates in normal proxy dep tracking — same `track` / `trigger` path as
any component field. In Rust, `store::<Preferences>()` returns a
`Handle<Preferences>` (the same `Handle<T>` that `this::<T>()` returns for a
component), exposing `update` / `with` closures over the concrete state.

## Current constraints

- **Reactivity is per field, by name.** `"count"` is a string key matched
  against the component's declared fields. There is no nested-field
  tracking, no array-element tracking, and no index tracking in collections.
- **Handler mutations trigger every key in scope.** Fine for small
  components; for a component with many cold fields and one hot one, prefer a
  `pp-model`-driven proxy assignment so the write goes through the
  single-field `trigger` path instead of the blanket `trigger_scope`, or
  reach for a `patch_*_inline` op to fire only the touched field.
- **`trigger_scope` is O(k)**, not O(n), thanks to the nested `DEPS` map,
  but it still fans out to all currently tracked keys. A handler that touches
  one field out of twenty will still rerun all twenty effects that tracked
  any field in the scope.
- **Scheduler is single-tier.** Flush runs all queued effects in one
  unordered batch. There are no pre/post/idle priority groups.

What's planned to lift these limits is tracked in [Roadmap](./05-roadmap.md).
