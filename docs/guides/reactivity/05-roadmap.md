---
title: "Reactivity roadmap"
description: "Which reactive primitives are shipped, which are in progress, and what's still ahead."
---

# Reactivity roadmap

This page tracks the state of the reactive layer — what's implemented in
`pocopine-core`, what's still pending, and why each item is scoped the way
it is. The model is signal-backed struct fields; see
[Overview](./README.md) for the shape, [Essentials](./01-essentials.md)
for the everyday API, and [Internals](./03-internals.md) for the deep
mechanics.

## Shipped

These are live in `crates/pocopine-core/src/`.

### Signal-backed fields

Each field of a `#[component]` is backed by an interned signal in one
unified dependency graph. Reads `track(scope_id, key)` through the
Rust-side access layer; writes `trigger(scope_id, key)`. A plain `pub`
struct field is the reactive unit — no wrapper, no cell. A `js_sys::Proxy`
exists only as a lazily-minted JS-interop shim (`js_bridge`); components
whose compiled plan is proxy-free mount without one. See `scope.rs` and
`reactive.rs`, or [Internals](./03-internals.md) for the read/write
lifecycle.

### Per-field handler triggers (dirty sweep)

Handler mutations on `&mut self` bypass the access layer, so
`Scope::invoke` fingerprints every observed field before the call (Fnv64
over a serde stream, entirely Rust-side) and re-fingerprints after: only
fields whose hash moved are invalidated and triggered. A handler touching
one field out of twenty re-runs one field's effects. The blanket
`trigger_scope` survives only as the fallback for re-entrant invokes and
unfingerprintable keys. Shipped in RFC-095 W2.

### Derived fields — `#[computed]`

`#[computed]` methods are static (no `self`), take the fields they depend
on as parameters, and are exposed to templates as read-only synthetic
fields. The runtime recomputes them when an input changes.

```rust
#[handlers]
impl PineUploadItem {
    #[computed]
    pub fn progress_label(progress: f64) -> String {
        format!("{}%", (progress * 100.0).round() as i32)
    }
}
```

Under the hood this is the `computed(f)` primitive in `computed.rs`
(documented in [Utilities](./02-utilities.md)): a `Computed<T>` that
re-evaluates `f` lazily — only when a dependency has changed *and* someone
reads the result again. It is a lazy effect with a custom scheduler that
flips a `dirty` flag and re-notifies the computed's own subscribers; the
body re-runs only when a caller reads the dirty value. `Computed<T>`
releases its backing effect on drop.

### Reacting to changes — `#[watch(field)]`

`#[watch(field)]` methods take `&mut self` and `(new: V, prev: Option<V>)`,
and run whenever the named field changes. The first call after mount
passes `None` for `prev`.

```rust
#[handlers]
impl Calendar {
    #[watch(value)]
    fn on_value_change(&mut self, new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.reflow();
    }
}
```

The lower-level free functions back this: `watch(source, cb)` over any
reactive read, and `watch_field("field", cb)` plus the `*_scoped` variants
that auto-release on unmount. Both `watch_field` forms defer install by
one microtask so the first read doesn't clash with the caller's active
`&mut self` borrow. See `watch.rs` and [Utilities](./02-utilities.md).

### `on_cleanup`

Registers a teardown closure on the currently running effect. The runtime
calls it before each rerun and on final `release`.

```rust
effect_scoped(|| {
    let handle = timers::every(1000, || { /* tick */ });
    on_cleanup(move || handle.cancel());
});
```

No-op when called outside an effect. See `reactive.rs`.

### `batch`

Coalesces multiple triggers into a single flush. Nestable.

```rust
use pocopine::{batch, store};

let cart = store::<Cart>();
batch(|| {
    cart.update(|c| c.add(item_a));
    cart.update(|c| c.add(item_b));
    // one flush, not two
});
```

See `reactive.rs`.

### Stores

`#[store]` declares a singleton component scope accessible as
`$store.<name>` in templates and as `store::<T>()` in Rust. Reactivity
works unchanged — `$store.<name>.<field>` reads resolve through the same
Rust-side access layer as component fields, so any directive reading
`$store.preferences.theme` re-evaluates when the field changes.

```rust
#[store]
#[derive(Default)]
pub struct Preferences {
    pub theme: String,
}
```

```poco
<div :class="$store.preferences.theme">...</div>
```

```rust
// Mutate from Rust:
store::<Preferences>().update(|p| p.theme = "dark".into());
```

See `store.rs` and `#[store]` in `pocopine-macros`.

### Keyed `pp-for`

RFC 054 shipped compiled row plans for `pp-for`. The macro records a
`StaticRowPlan` at compile time; the runtime reconciler uses it to key
rows by identity, move existing DOM nodes on reorder, and update only the
cells that changed. The `scope::patch_list_at_inline`,
`append_list_inline`, `swap_list_indices_inline`, and
`remove_list_at_inline` helpers let handlers update the cached JS Array
surgically so reconcile skips unchanged rows entirely — see
[Utilities](./02-utilities.md) for the full `scope::*_inline` family.

## Still pending

### Scheduler tiers

The effect queue is single-tier today. `tick::next` (microtask),
`tick::next_frame` (rAF), and `tick::after_flush` (macrotask) cover
deferred scheduling, but there is no pre-flush / post-flush grouping that
would let, for example, animations always run after all data effects have
settled.

### Nested reactivity

Field tracking is flat: `self.title` is one reactive key. Writing
`self.user.name.first` inside a handler correctly triggers `user` (the
dirty sweep sees the fingerprint move) but cannot notify a subscriber of
just that leaf — every `user` reader re-evaluates. True nested reactivity
requires per-path subscriptions with an "iteration" synthetic key so
inserts and deletes re-notify loop effects. This is on the critical path
for per-row reactive fields inside `pp-for`.

### Cheaper sweeps for huge cold fields

The dirty sweep hashes every observed field on every handler call. That
is one serde pass per field — cheap for scalars, measurable for a scope
holding a very large `Vec` nobody mutates. Possible follow-ups: skip-list
hints from the `#[handlers]` macro (static analysis of which fields a
method can touch), or length+generation short-circuits for collection
fields.
