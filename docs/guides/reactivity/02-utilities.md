---
title: "Reactivity utilities"
description: "The lower-level reactive toolbox — effects, batching, watchers, scheduling, timers, refs, context, surgical list updates, and standalone signals."
---

# Reactivity utilities

Most components never reach here. A plain struct field plus `#[computed]` /
`#[watch(field)]` covers the everyday cases — see [Essentials](./01-essentials.md).

Reach for this toolbox when you need **standalone reactive work** that isn't a
component field: an `effect` over a `#[store]`, a custom directive, library
code, a debounced timer, a scheduled DOM read, or a surgical update to a big
`Vec<T>`. These are lower-level primitives — the same ones the component macros
compile down to.

## Where things live

Import path is the fastest way to find a tool. The everyday primitives are in
the prelude; the rest live at the crate root or in a module.

| Tool | Import |
|---|---|
| `effect`, `batch`, `computed`, `watch`, `on_cleanup`, `signal`, `rw_signal` | `use pocopine::prelude::*` |
| `effect_scoped`, `effect_with`, `EffectOptions`, `watch_field`, `*_scoped` variants | `use pocopine::{effect_scoped, effect_with, EffectOptions, …}` |
| `provide`, `inject`, `flush_sync`, `set_auto_flush`, the `*_inline` list ops | `use pocopine::{provide, inject, flush_sync, …}` |
| `tick::next` / `next_frame` / `after_flush` | `use pocopine::tick` |
| `timers::after` / `every` / `Debounced` / `sleep` | `use pocopine::timers` |
| `refs::get` / `get_as` / `get_component` | `use pocopine::refs` |

`create_context!` is a macro and is in the prelude.

## Effects — `effect` / `effect_scoped`

`effect(f)` runs `f` immediately, records every reactive read as a dependency,
and re-runs `f` on the next microtask whenever any of those deps change. It
returns an `EffectId` you can later `release`.

```rust
use pocopine::prelude::*;

// Re-runs whenever the Cart store's contents change.
let cart = store::<Cart>();
let id = effect(move || {
    let total = cart.with(|c| c.total());
    tracing::info!(target: "pocopine.log", "cart total: {total}");
});
// later, when you're done with it:
release(id);
```

Inside a component, prefer **`effect_scoped`** — it installs the effect and
registers a release against the current scope's unmount, so you never have to
store the id or call `release` yourself. It returns nothing; storage is
implicit.

`on_cleanup(f)` registers a teardown that runs **before each rerun** and on the
final release — the place to cancel timers, subscriptions, and listeners so they
never leak across reruns.

```rust
use pocopine::{effect_scoped, timers};
use pocopine::prelude::*;

effect_scoped(|| {
    // A fresh interval each run; the previous one is cancelled first.
    let handle = timers::every(1000, || {
        tracing::info!(target: "pocopine.log", "tick");
    });
    on_cleanup(move || handle.cancel());
});
```

`effect_scoped` / `on_cleanup` panic-free outside an effect: `on_cleanup` is a
no-op when there's no enclosing effect, and `effect_scoped` reads the current
scope for its unmount hook.

## Batching — `batch`

`batch(f)` coalesces every write inside `f` into a single flush. It's nestable
— the flush fires only when the outermost batch exits, and only if something
actually queued.

```rust
use pocopine::prelude::*;

let cart = store::<Cart>();
batch(|| {
    cart.update(|c| c.add(item_a));
    cart.update(|c| c.add(item_b));
});
// one rerun of every subscriber, not two
```

`batch` returns whatever `f` returns, so it composes inside expressions.

## Memoized derivations — `computed()`

`computed(f)` is the free function behind `#[computed]`. It builds a
`Computed<T>` — a **lazy, memoized** derivation. `f` runs the first time you
call `.get()`, and only re-runs when an input changed *and* something reads the
result again. Reading `.get()` subscribes the calling effect, so a `computed`
can be a dep like any other reactive read.

```rust
use pocopine::prelude::*;

let (a, set_a) = signal(2_i32);
let doubled = computed(move || a.get() * 2);

assert_eq!(doubled.get(), 4);   // first read runs the body
assert_eq!(doubled.get(), 4);   // cached — body did not run again

set_a.set(10);
assert_eq!(doubled.get(), 20);  // dep changed → recomputes on read
```

Dropping the `Computed<T>` releases its underlying effect, so it cleans up with
no manual teardown. Inside a component you almost always want `#[computed]`
instead — see [Essentials](./01-essentials.md).

## Watchers — `watch` / `watch_field` / `*_scoped`

The `watch` family is the free-function form of `#[watch(field)]`. Each calls a
callback with `(new, prev: Option<&T>)` — `prev` is `None` on the first run,
then `Some` on each distinct subsequent value (equality checked with
`PartialEq`).

`watch(source, cb)` watches an arbitrary reactive read:

```rust
use pocopine::prelude::*;

let sig = rw_signal(5_i32);
watch(
    move || sig.get(),
    |next, prev| {
        tracing::info!(target: "pocopine.log", "value: {prev:?} -> {next}");
    },
);
```

`watch_field::<V>("field", cb)` is the ergonomic sugar for "watch one named
field on the current component." It reads the field **through the tracked
access layer** so the effect subscribes correctly — the common pitfall with a
hand-rolled `watch(|| handle.with(|s| s.field), …)` is that `Handle::with`
bypasses tracking, so the watch silently never fires.

```rust
use pocopine::watch_field;

// inside a handler / lifecycle method:
watch_field::<bool>("open", |&is_open, prev| match (prev, is_open) {
    (None, true) | (Some(false), true) => activate(),
    (Some(true), false) => deactivate(),
    _ => {}
});
```

`watch_field` reads `current_scope_id()` at install time and panics outside a
handler / lifecycle context. Its install is deferred one microtask so the
initial read doesn't clash with the caller's active `&mut self` borrow.

| Variant | When |
|---|---|
| `watch(source, cb)` | Watch any reactive closure. Returns an `EffectId`. |
| `watch_field::<V>(field, cb)` | Single named field on the current scope. |
| `watch_scope_field::<V>(scope, field, cb)` | A field on an explicit scope (e.g. a provided parent). |
| `watch_scoped` / `watch_field_scoped` / `watch_scope_field_scoped` | As above, but auto-released at the current scope's unmount. |

Use the `*_scoped` variants inside lifecycle hooks — they tie the watcher's
release to unmount so you don't manage the `EffectId`.

## Custom scheduling — `effect_with` + `EffectOptions`

`effect_with(f, EffectOptions { lazy, scheduler })` is the low-level primitive
both `effect` and `computed` build on. Two knobs:

- `lazy: true` — register the effect but **don't run it** until something
  schedules it (via `run_now`).
- `scheduler: Some(f)` — when set, a `trigger` hands control to your closure
  `f(EffectId)` instead of queueing the effect for the microtask flush.

That combination is exactly how `computed` works: a lazy effect whose scheduler
flips a `dirty` bit and re-notifies the computed's own subscribers.

```rust
use std::rc::Rc;
use pocopine::{effect_with, run_now, EffectOptions, EffectId};

let id = effect_with(
    || recompute(),
    EffectOptions {
        lazy: true,
        scheduler: Some(Rc::new(|eid: EffectId| {
            // a dep changed — decide when to actually re-run
            mark_dirty(eid);
        })),
    },
);
run_now(id); // run on demand
```

Most code never needs this directly — reach for it only when you're building a
derivation primitive of your own. See [Internals](./03-internals.md) for how the
scheduler hook routes through `trigger`.

## Scheduling work — `tick`

`tick` defers a one-shot closure to a specific point relative to the reactive
flush. Pick by *when* you need the DOM to be ready.

| Fn | Slot | Use for |
|---|---|---|
| `tick::next(f)` | next microtask (`queueMicrotask`) | reactive DOM updates have landed, before paint — focus an input after a dialog mounts |
| `tick::next_frame(f)` | next `requestAnimationFrame` | layout / computed style must be resolved — `getBoundingClientRect` after a transition class applies |
| `tick::after_flush(f)` | macrotask after the flush (`setTimeout(0)`) | strictly later than `next` — re-focus an element the reactive walk just blurred |

```rust
use pocopine::tick;

// Focus the target after it has actually mounted.
pub fn focus_after_flush(selector: &'static str) {
    tick::after_flush(move || {
        if let Some(el) = pocopine::dom::document()
            .and_then(|d| d.query_selector(selector).ok().flatten())
        {
            pocopine::focus::focus_element_no_scroll(&el);
        }
    });
}
```

> Scope context does not survive a deferred callback: `current_scope_id()` and
> `refs::get` return `None` inside a `tick::next` continuation. Resolve any DOM
> refs synchronously and capture them in the closure.

## Timers — `timers`

`timers` wraps `setTimeout` / `setInterval` behind a small RAII surface and
ties timer lifetimes to scope unmount. Handles cancel on drop; `*_scoped`
variants cancel at the current scope's unmount with nothing to store.

| Fn | Returns | Lifetime |
|---|---|---|
| `after(ms, f)` | `TimeoutHandle` | cancels on drop / `.cancel()` |
| `after_scoped(ms, f)` | — | cancels at unmount |
| `every(ms, f)` | `IntervalHandle` | cancels on drop / `.cancel()` |
| `every_scoped(ms, f)` | — | cancels at unmount |
| `Debounced` | `Rc<Debounced>` | cancel-and-replace slot |
| `sleep(ms).await` / `next_frame().await` / `next_tick().await` | future | drop the future to abort |

A scope-bound interval that simulates a server pushing live data, then fires a
one-shot to clear a flag — both auto-cancel when the component unmounts:

```rust
use pocopine::prelude::*;
use pocopine::timers;

pub fn on_setup(&mut self) {
    let handle = this::<Self>();
    timers::every_scoped(3200, move || {
        if handle.update(|c| c.server_push()) {
            handle.update(|c| c.syncing = true);
            let h = handle.clone();
            timers::after_scoped(900, move || h.update(|c| c.syncing = false));
        }
    });
}
```

`Debounced` is the cancel-and-replace slot for hover / scroll / autosave — each
`schedule` cancels the prior pending fire. Build it with `new_scoped()` so it
auto-cancels at unmount, then clone the `Rc` into each handler:

```rust
use pocopine::timers::Debounced;

let pending = Debounced::new_scoped();
pending.schedule(700, move || root.update(|s| s.open = true));
// a later call cancels the 700ms fire and replaces it:
pending.cancel();
```

For mid-flight cancellation that composes with `await`, reach for the awaitable
helpers inside a `spawn_scoped(async move { … })` — the in-flight `await` is
dropped (and the underlying timer becomes a no-op) when the scope unmounts.

## Element refs — `refs`

`pp-ref="name"` pins an element under its scope so a Rust handler can reach it
imperatively. `refs::get(name)` resolves it against the current handler's scope;
`get_as::<T>` downcasts; `get_component::<T>` resolves a `Handle<T>` for a child
component's host.

```rust
use pocopine::refs;
use web_sys::HtmlInputElement;

// inside a handler / lifecycle method:
if let Some(input) = refs::get_as::<HtmlInputElement>("search") {
    let _ = input.focus();
}
```

Refs are scoped — the same name in two sibling components doesn't collide — and
clear on scope teardown. Resolve them synchronously (see the `tick` note above).
Full coverage in [Component lifecycle](../components/04-lifecycle.md).

## Context — `provide` / `inject`

`provide(&KEY, value)` stores a value on the current scope; `inject(&KEY)` walks
up the scope-parent chain and returns a clone of the first match. The key is a
typed `ContextKey<T>` declared once with `create_context!`, so `inject` returns
`Option<T>` with no turbofish. Parent links are tracked by scope (not the DOM),
so teleported and slotted children still resolve to their authoring parent.

```rust
use pocopine::create_context;
use pocopine::prelude::*;

create_context!(pub(crate) NOTE_FORM_CONTEXT: NoteFormContext);

// provider — in a parent's on_setup:
NOTE_FORM_CONTEXT.provide(NoteFormContext::EDITOR_MODAL);

// consumer — in a descendant's on_setup:
let ctx = NOTE_FORM_CONTEXT
    .inject()
    .unwrap_or(NoteFormContext::DRAFT_COMPOSER);
```

Both `provide` and `inject` panic outside a handler / lifecycle context — a call
that can't identify its scope is always a programming error. The key carries `T`
at the type level, so the value type is checked at both ends. For cross-component
*state* (not config), prefer `#[store]` — see [Essentials](./01-essentials.md).

## Surgical list updates — `scope::*_inline`

A handler mutation of a `Vec<T>` field triggers that field (the dirty sweep
sees the fingerprint move) — but the next read re-serialises the **whole**
field into a fresh JS `Array`, giving every row a new object identity. The
`*_inline` helpers instead patch the cached JS `Array` in lockstep with the
Rust mutation, so a keyed `pp-for` reconcile sees the same Array with one
updated cell and skips the untouched rows entirely.

Call them from inside a `&mut self` handler, *after* the Rust-side mutation:

```rust
use pocopine::patch_list_at_inline;

pub fn touch_row(&mut self, idx: usize) {
    self.rows[idx].label.push_str(" !!!");
    // patch the cached JS Array cell + trigger only "rows"
    patch_list_at_inline("rows", idx, &self.rows[idx]);
}
```

| Fn | For |
|---|---|
| `patch_list_at_inline(field, idx, &row)` | one changed element |
| `patch_list_indices_inline(field, &[(idx, &row)])` | several changed elements, one trigger |
| `append_list_inline(field, start_idx, &rows)` | extend the Vec |
| `prepend_list_inline(field, &rows)` | prepend, preserving shifted-tail identities |
| `remove_list_at_inline(field, idx)` | remove one element |
| `swap_list_indices_inline(field, a, b)` | swap two indices |
| `replace_field_inline(field, &value)` | structural reshape — re-serialise the whole field once |

These are a perf escape hatch — only worth it for large lists under reactive
pressure. A plain `self.rows[idx] = …` is correct everywhere else; it just
pays one whole-field re-serialisation on the next read. See
[Internals](./03-internals.md) for the projection-store mechanics.

## Testing the reactive loop — `set_auto_flush` + `flush_sync`

In production, a `trigger` schedules a microtask flush automatically. Tests that
want deterministic control disable that and drive the flush by hand — which also
side-steps environments (e.g. `wasm-pack test --node`) where `spawn_local` has
no microtask host.

```rust
use pocopine::{set_auto_flush, flush_sync, effect};
use pocopine::prelude::*;

set_auto_flush(false);              // queue, don't auto-flush

let (s, setter) = signal(0_i32);
let seen = std::rc::Rc::new(std::cell::Cell::new(-1));
let seen_w = seen.clone();
let s_c = s.clone();
effect(move || seen_w.set(s_c.get()));
assert_eq!(seen.get(), 0);          // effect ran once on install

setter.set(3);
flush_sync();                        // drain the queue now
assert_eq!(seen.get(), 3);
```

With auto-flush off, queued effects stay parked until `flush_sync()` drains
them. `batch` still coalesces; `flush_sync` just replaces the microtask.

## Advanced: standalone signals

> **Escape hatch — not the everyday tool.** For state owned by a component, use
> a plain struct field; for app-wide state, use `#[store]`. Signals exist for
> the rare case of **reactive state that isn't tied to a component** — library
> code, a module-level reactive value, a bridge wrapping an external source.
> Don't reach for them as a general state mechanism. The
> [Vue 3 reference](./04-vue3-reference.md) maps `signal()` to Vue's `ref()` and
> explains why it's deliberately rare here.

A signal is a typed reactive cell. `signal(initial)` returns a split
`(Signal<T>, Setter<T>)` pair; `rw_signal(initial)` returns a combined
`RwSignal<T>`. Reading via `.get()` subscribes the current effect; writing via
`.set()` notifies subscribers — but only when the value actually changed
(`PartialEq`), which kills a class of feedback-loop bugs.

```rust
use pocopine::prelude::*;

let (count, set_count) = signal(0_i32);

// reads inside an effect subscribe to the signal:
effect({
    let count = count.clone();
    move || tracing::info!(target: "pocopine.log", "count is {}", count.get())
});

set_count.set(1);   // re-runs the effect on the next flush
set_count.set(1);   // same value → no trigger
```

| Type | Construct | Read | Write |
|---|---|---|---|
| `Signal<T>` | `let (s, set) = signal(v)` | `s.get()` / `s.with(\|v\| …)` | — (read-only) |
| `Setter<T>` | (the pair's second half) | — | `set.set(v)` / `set.set_force(v)` / `set.update(\|v\| …)` |
| `RwSignal<T>` | `let s = rw_signal(v)` | `s.get()` / `s.with(\|v\| …)` | `s.set(v)` / `s.update(\|v\| …)`; `s.split()` → pair |

`set` skips the trigger when the new value equals the old; `set_force` re-fires
unconditionally; `update(|v| …)` mutates in place and always fires (a closure
can't prove it didn't change the value). Signals and component fields share
ONE dependency graph: a field is just a signal the runtime interned for its
`(ScopeId, key)` pair, while `signal()` allocates the id directly. Same
effect engine, queue, flush, and batching; `computed` rides the same graph.
See [Internals](./03-internals.md).
