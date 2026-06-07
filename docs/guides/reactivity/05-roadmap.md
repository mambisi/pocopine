---
title: "Reactivity roadmap"
description: "Which reactive primitives are shipped, which are in progress, and what's still ahead."
---

# Reactivity roadmap

This page tracks the state of the reactive layer — what's implemented in
`pocopine-core`, what's still pending, and why each item is scoped the way
it is. The model is proxy-scoped struct fields; see
[Overview](./README.md) for the shape, [Essentials](./01-essentials.md)
for the everyday API, and [Internals](./03-internals.md) for the deep
mechanics.

## Shipped

These are live in `crates/pocopine-core/src/`.

### Proxy-scoped fields

Each `#[component]` is wrapped in a `js_sys::Proxy`. Reads through the
proxy `track(scope_id, key)`; writes `trigger(scope_id, key)`. A plain
`pub` struct field is the reactive unit — no wrapper, no cell. Handler
mutations on `&mut self` bypass the proxy and fan out via `trigger_scope`
after the method returns. See `scope.rs` and `reactive.rs`, or
[Internals](./03-internals.md) for the read/write lifecycle.

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
works unchanged — the store's proxy `get` trap tracks dependencies so any
directive reading `$store.preferences.theme` re-evaluates when the field
changes.

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

### Fine-grained handler triggers

After a `#[handlers]` method returns, `Scope::invoke` calls
`trigger_scope`, which notifies every currently-tracked key on the scope.
That is correct but coarser than necessary: a handler that touches only
`count` still re-evaluates effects subscribed to `title`, `items`, and
every other field.

The fix requires the `#[handlers]` macro to analyse `&mut self.field`
assignments and emit per-field `trigger` calls instead of a blanket
`trigger_scope`. The main constraint is reliable pattern matching across
all assignment forms — direct field writes, nested method calls, `Deref`
targets.

### Scheduler tiers

The effect queue is single-tier today. `tick::next` (microtask),
`tick::next_frame` (rAF), and `tick::after_flush` (macrotask) cover
deferred scheduling, but there is no pre-flush / post-flush grouping that
would let, for example, animations always run after all data effects have
settled.

### Nested reactivity

Field tracking is flat: `self.title` is one reactive key; writing
`self.user.name.first` does not automatically invalidate a subscriber of
`self.user`. Nested reactivity requires deep proxy wrapping with an
"iteration" synthetic key so inserts and deletes re-notify loop effects.
This is on the critical path for per-row reactive fields inside `pp-for`.
