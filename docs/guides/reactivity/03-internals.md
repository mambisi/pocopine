---
title: "Reactivity internals"
description: "How pocopine's signals-first runtime tracks dependencies, fingerprints handler writes, schedules updates, and exposes computed values and stores — for people extending the engine."
---

# Reactivity internals

This is the engine room. For the everyday API see
[Essentials](./01-essentials.md) and [Utilities](./02-utilities.md); for the
overview and routing table start at the [Overview](./README.md). This page
is for extenders who need to know how the runtime actually works.

## Big picture

Pocopine has no virtual DOM and no render pass. A directive is just an
**effect** — a closure registered with the reactive engine. When that
closure runs it reads component fields through the **scoped access** layer
(Rust-side, no JS proxy on the path); each read **subscribes** the running
effect to that field's interned **signal**. When the field is written, the
engine **requeues** every subscribed effect and reruns them on the next
microtask. There is no diffing — the effect re-reads the value and patches
its own piece of the DOM.

```text
read  →  read_field_tracked(scope, key)  →  track: intern (scope, key) → SignalId
                                            subscribe running effect to it
write →  write_field_tracked(scope, key) →  trigger: bump the field's projection
                                            version, queue subscribers
                                            →  microtask flush → rerun
```

A `js_sys::Proxy` still exists — but as an **optional JS-interop shim**,
not the engine. Components whose compiled plan is provably proxy-free mount
without one (`StaticTemplatePlan::needs_proxy`); the proxy is lazy-minted
only when something genuinely dynamic asks for it, and its get/set traps
are thin wrappers over the same `read_field_tracked` / `write_field_tracked`
mirrors every other reader uses. `js_bridge(scope_id)` is the explicit way
to get one for JS interop.

Three moving parts hold this together, and the rest of this page walks each:

- **One signal graph.** Every dependency edge — component field, store
  field, standalone signal, `computed` result — lives in a single id-keyed
  table. Component fields get there by **interning**: the first
  `track(scope, key)` allocates a `SignalId` for that `(scope, field)` pair.
- **An effect engine.** Effects, their schedulers, cleanups, and reverse
  dep edges, all keyed on `EffectId`.
- **A microtask flush.** A single coalesced drain of the queue, scheduled
  via a resolved-promise microtask.

Everything below lives in `crates/pocopine-core/src/reactive.rs` (the
engine), `scope.rs` (the access mirrors, projections, and handler
dispatch), and `fingerprint.rs` (the dirty-sweep hasher), with
`computed.rs`, `signal.rs`, and `store.rs` as consumers.

## Thread-locals

Everything lives in per-wasm-module `thread_local`s. WASM is single-threaded,
so these are effectively module-globals with safe access from any call site.

In `reactive.rs` (the engine):

| Thread-local | Type | Role |
|---|---|---|
| `NEXT_ID` | `Cell<u64>` | Monotonic id source shared by scopes, effects, and signals. Starts at `1`; `0` stays unallocated as an easy-to-spot "never minted" value. |
| `CURRENT_EFFECT` | `Cell<Option<EffectId>>` | The effect running right now. Read by `track` / `track_signal` to know who is subscribing. |
| `EFFECTS` | `HashMap<EffectId, Rc<dyn Fn()>>` | The effect body. Rerun by id during flush. |
| `SCHEDULERS` | `HashMap<EffectId, Rc<dyn Fn(EffectId)>>` | Per-effect custom schedulers. When present, dispatch calls the scheduler inline instead of queueing. Used by `computed` to flip its dirty bit. |
| `FIELD_SIGNALS` | `HashMap<ScopeId, HashMap<Key, SignalId>>` | The interning table: which `SignalId` represents each `(scope, field)` pair. The nesting keeps `trigger_scope` and `DirtySweep::begin` O(k) in the scope's observed keys, and lets lookups probe with a bare `&str`. |
| `SIGNAL_DEPS` | `HashMap<SignalId, HashSet<EffectId>>` | THE forward map: given a signal, which effects to rerun. Fields, stores, standalone signals, and `computed` results all live here. |
| `SIGNAL_REVERSE` | `HashMap<EffectId, HashSet<SignalId>>` | The back map: given an effect, which signals it subscribed to. Used by `clear_deps_for` to drop stale subscriptions before each rerun. |
| `QUEUE` | `HashSet<EffectId>` | Effects pending rerun. Drained by `flush`. |
| `FLUSH_SCHEDULED` | `Cell<bool>` | Guards against scheduling more than one flush microtask at a time. |
| `CLEANUPS` | `HashMap<EffectId, Vec<Box<dyn FnOnce()>>>` | Teardown hooks registered via `on_cleanup`. Run before the next rerun or when `release` is called. |
| `BATCHING` | `Cell<u32>` | Nestable batch counter. Flush is deferred until the outermost `batch` completes. |
| `AUTO_FLUSH` | `Cell<bool>` | Disabling this (tests only) holds queued effects until `flush_sync` runs. |
| `TRIGGER_SCRATCH` | `RefCell<Vec<EffectId>>` | Reusable per-thread snapshot buffer so dispatch does not clone a `HashSet` on every trigger. |

In `scope.rs` (access, projections, and the proxy shim):

| Thread-local | Type | Role |
|---|---|---|
| `SCOPES` | `HashMap<ScopeId, Scope>` | The scope registry: id → state + lazily-minted proxy slot. |
| `VERSIONS` | `HashMap<SignalId, u32>` | Per-field projection version. A write bumps it; that bump IS the cache invalidation. |
| `PROJECTIONS` | `HashMap<SignalId, (u32, JsValue)>` | The serde projection store: the field's `JsValue` form, stamped with the version it was built at. Stale stamp = rebuild on next read. |
| `PATCHED` | `HashMap<ScopeId, HashSet<SignalId>>` | Fields surgically patched via `patch_*_inline` since the last sweep. The sweep re-stamps these to the new version instead of invalidating — the patch already wrote the JS value correctly. |
| `BRIDGES` | `HashMap<ScopeId, JsValue>` | Memoized `js_bridge` proxies for explicit JS interop. |
| `PROXIES_MINTED` / `SERDE_PROJECTIONS` | `Cell<u64>` | Observability counters (`proxies_minted_count`, `serde_projection_count`) — the elision and typed-lane acceptance gates assert against them in tests. |
| `CURRENT_SCOPE_ID` / `CURRENT_EL` | — | Ambient context during handler invocation and directive install, for `this::<T>()`, `dispatch!`, and `refs`. |

The `Key` type is `Cow<'static, str>`: a macro-generated `&'static str`
threads through to the `HashMap` without allocation, while a dynamically
built key owns its string exactly once. `HashMap`'s `Borrow` support lets
lookups probe with a bare `&str`.

## One graph, interned fields

There is **one** dependency table. Component fields join it by interning:

```text
track(scope_id, "count")
  └─ ensure_field_signal(scope_id, "count")
     └─ FIELD_SIGNALS[scope][("count")]   — allocate SignalId on first use
  └─ track_signal(sid)
     ├─ SIGNAL_DEPS[sid].insert(current_effect)
     └─ SIGNAL_REVERSE[current_effect].insert(sid)
```

Standalone `signal` / `rw_signal` and `computed` allocate their `SignalId`s
directly, so fields, stores, signals, and computed results are
indistinguishable to the dispatcher: `trigger(scope, key)` resolves the
interned id and calls the same `trigger_signal` tail that `Setter::set`
uses. An effect that reads a field *and* a signal has both edges in
`SIGNAL_REVERSE`; `clear_deps_for` clears them uniformly before every
rerun.

(Historically there were two tables — a string-keyed proxy map plus the
id-keyed signal map — unified in RFC-095 W3a. An interning experiment that
also cached `Key → SignalId` lookups per call site was benchmarked and
reverted; the two-level `FIELD_SIGNALS` probe is already cheap.)

## The lifecycle of a read

```text
directive body runs inside effect(f)
  └─ CURRENT_EFFECT = Some(id)
     └─ read_field_tracked(scope_id, state, "count")
        ├─ track(scope_id, "count")          intern + subscribe (see above)
        └─ value, in priority order:
           ├─ typed text lane: field_as_text(key) — scalar fields
           │  (String / numbers / bool) stringify Rust-side, zero serde
           ├─ projection store: PROJECTIONS[sid] if its stamp == VERSIONS[sid]
           └─ rebuild: state.borrow().get("count") → serde_wasm_bindgen,
              stored back into PROJECTIONS at the current version
```

`track` short-circuits when the subscription already exists — the common
hot path of an effect re-reading fields it is already subscribed to bails
without any allocation.

Two details worth knowing:

- **The typed text lane** (`pp-text` and text interpolation on scalar
  fields) never builds a `JsValue` at all: `read_field_text` subscribes,
  then extracts a `String` straight from Rust state via the macro-generated
  `field_as_text` arms. The `serde_projection_count` counter exists to
  prove this in tests.
- **Versioned projections replace cache invalidation.** Nothing ever
  "clears the cache" — a write bumps `VERSIONS[sid]`, and the next read
  finds the projection's stamp stale and rebuilds. `patch_*_inline` APIs
  write the projection in place and mark the field in `PATCHED` so the
  next sweep re-stamps instead of bumping.

Derived scopes (`SlotScope`, `LoopScope`, `PayloadScope`) declare
themselves non-cacheable (`cacheable_fields() == false`) — they recompose
values from a parent scope on every read, so projecting would freeze them.
Their `get` falls through to `read_scope_key(parent_scope_id, key)` for
anything they don't own, which keeps loop bodies and slot content
read-complete without any proxy.

## The lifecycle of a write

Three write paths, all converging on `trigger`.

**Through the write mirror** — `pp-model` input events, template
assignments (`open = !open`), and the proxy set trap all call the same
function:

```text
write_field_tracked(scope_id, state, "count", 3)
  ├─ state.borrow_mut().set("count", 3)
  ├─ VERSIONS[sid] += 1                       (projection invalidated by stamp)
  └─ trigger(scope_id, "count")
     └─ trigger_signal(FIELD_SIGNALS[scope]["count"])
        └─ dispatch_subs(SIGNAL_DEPS[sid])
           ├─ effects with a custom scheduler: scheduler(effect_id) inline
           └─ remaining effects: QUEUE.insert(effect_id)
        └─ schedule_flush()
```

When the field is a `flatten` leaf, the mirror also resolves the container
key and triggers it too — so `#[watch(<container>)]` fires alongside the
per-leaf watch.

**Through a handler** — `#[handlers] fn increment(&mut self)` mutates Rust
state directly, bypassing every mirror. The runtime can't see which fields
a `&mut self` method changed, so `Scope::invoke` brackets the call with a
**dirty sweep** (RFC-095 W2):

```text
Scope::invoke("increment")
  ├─ DirtySweep::begin
  │  └─ for every interned key of the scope:
  │     before[k] = state.field_fingerprint(k)     (Fnv64 over a serde stream,
  │                                                 no JS, no allocation)
  ├─ state.borrow_mut().invoke("increment", args)  (your &mut self code)
  └─ DirtySweep::finish
     ├─ re-fingerprint; changed = keys whose hash moved
     ├─ changed + patched   → projection re-stamped (PATCHED consumed)
     ├─ changed + unpatched → version bump
     └─ trigger(scope, k) for each changed k       — and ONLY those
```

A handler that touches one field out of twenty triggers exactly one field.
Keys whose fingerprint is unavailable (`None` — computed fields, certain
flatten shapes) are conservatively treated as changed. If the sweep can't
even snapshot (a re-entrant invoke holding the state borrow), the runtime
falls back to the blanket path: invalidate every projection and
`trigger_scope` — every tracked key of the scope. The fallback is the
exception, not the model.

**`Handle::update` / `dispatch!`** take the same sweep-bracketed path as
handler invocation.

## Dispatch and flushing

`dispatch_subs` is the shared tail of every trigger. It snapshots the
subscriber set into the `TRIGGER_SCRATCH` buffer (taking ownership, so a
re-entrant dispatch from an inline scheduler doesn't clobber the outer
call's snapshot), then for each subscriber: if it has a custom scheduler,
call it inline; otherwise push it onto `QUEUE`. If anything was queued and
no batch is open, it calls `schedule_flush`.

`schedule_flush` spawns a microtask via `wasm_bindgen_futures::spawn_local`,
awaiting `JsFuture::from(Promise::resolve(&JsValue::NULL))`. When the
resolved promise settles, `flush` drains the queue and reruns each pending
effect. `FLUSH_SCHEDULED` ensures at most one such microtask is in flight
at a time.

Re-running an effect (`run_effect`):

1. Run all `on_cleanup` hooks registered during the previous run — a cleanup
   registered on iteration N belongs to N, not N+1.
2. `clear_deps_for(id)` — removes this effect's edges from the signal graph.
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

## The proxy, demoted to a shim

The compiled template plan records `needs_proxy: bool`. The classifier
marks a plan proxy-free when every install is provably servable by the
access layer — bindings, interps, refs, listeners, native models,
conditional chains and `pp-match` with proxy-free bodies, and `pp-for`
sites whose items expression isn't `$`-rooted and whose `pp-key` is
item-rooted. For those components, mount **mints nothing** — the
`proxies_minted_count` counter stays flat, which the acceptance tests
assert.

When something genuinely dynamic needs a JS object later —
`scope_of_element`, a delegated row listener firing, JS interop — the
proxy is **lazy-minted** and cached on the scope. Its get trap calls
`read_field_tracked`, its set trap calls `write_field_tracked`: the same
mirrors everything else uses, so a proxy-mediated read/write is
behaviorally identical to an elided one, just one JS hop slower.

`js_bridge(scope_id)` is the explicit, memoized entry point for handing a
component's state to JS code. Use it instead of fishing the proxy out of
mount internals.

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
reads the result again. Dropping the `Computed<T>` releases the underlying
effect via `release`, so parent effect scopes stay tidy without a manual
teardown step.

(A push-pull upgrade in the style of alien-signals — colors/graph-walk
instead of eager dirty notification — was profiled during RFC-096 S5 and
not adopted: the dispatch tail measured 0.0ms on the heaviest benchmark
action, so there is nothing for the added complexity to win.)

## Signals internals

A signal is a `(Signal<T>, Setter<T>)` pair (or a combined `RwSignal<T>`)
sharing one `Rc<RefCell<T>>` and one `SignalId`. `get` / `with` call
`track_signal`; `set` / `update` call `trigger_signal`. `Setter::set` and
`RwSignal::set` carry a `PartialEq` value-equality guard — a write equal to
the current value is a no-op, which prevents a class of watcher-thrash and
render-loop bugs where an effect writes back a value it just read. The
`set_force` / `update` variants fire unconditionally.

Signals are the [standalone escape hatch](./02-utilities.md#advanced-standalone-signals),
not the everyday model — component fields ride the same graph, so there is
no engine-level reason to prefer one over the other; pick by ownership.

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
`$store.preferences.theme` resolves through `magic_scope_access` — the
same Rust-side read mirror as component fields, subscribing to the store
scope's interned field signal; no proxy on the read path. In Rust,
`store::<Preferences>()` returns a `Handle<Preferences>` (the same
`Handle<T>` that `this::<T>()` returns for a component), exposing
`update` / `with` closures that run inside a dirty sweep like any handler.

## Current constraints

- **Reactivity is per field, by name.** `"count"` is a string key matched
  against the component's declared fields. There is no nested-field
  tracking, no array-element tracking, and no index tracking in
  collections — but the dirty sweep means a handler mutation of
  `self.rows[3].label` still triggers only `rows`, not the whole scope.
- **Fingerprinting costs one serde pass per observed field per handler
  call.** Fnv64 over a serde stream is cheap, but a scope with a huge
  cold field (a big `Vec` nobody mutates) still pays to hash it on every
  handler invocation. The `patch_*_inline` ops sidestep the projection
  rebuild but not the hash.
- **`trigger_scope` still exists as the fallback** for re-entrant invokes
  and unfingerprintable keys — code extending the engine must keep it
  correct even though the sweep makes it rare.
- **Scheduler is single-tier.** Flush runs all queued effects in one
  unordered batch. There are no pre/post/idle priority groups.

What's planned to lift these limits is tracked in [Roadmap](./05-roadmap.md).
