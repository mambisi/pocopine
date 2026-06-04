---
title: "Reactivity"
description: "The reactive core: the JS Proxy bridge, effects, dependency tracking, computed values, and watchers."
---

# Reactivity

Pocopine connects your Rust component state to the DOM without a virtual
DOM or explicit render calls. Every `#[component]` is wrapped in a
`js_sys::Proxy`. When a directive reads a field through that proxy it
subscribes; when a handler writes that field, every subscribed directive
re-evaluates — nothing more, nothing less.

This is the same proxy-based model Vue 3 calls `reactive()`, and the proxy
and effect engine are native browser machinery. There is **no `useState`,
no reactive cell to wrap your values in** — a plain struct field *is* the
reactive primitive.

```rust
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

```poco
<button @click="increment">count is <span pp-text="count"></span></button>
```

`self.count += 1` is the whole story: the handler mutates a field, and the
one `pp-text` that read `count` re-runs on the next microtask.

## Holding state

Reaching for a reactive "primitive" is almost never the answer. Pick by
what owns the state — see [State management](../components/02-state.md)
for the full decision table:

| State | Pattern |
|---|---|
| Owned by one component | a struct field |
| Passed parent → child | `pp-bind:` attributes |
| Reported child → parent | `emit(name, detail)` + `@event` |
| Shared across the app | `#[store]` |

## Deriving state

Derived display state is also a field — kept up to date for you. Don't
mirror it by hand in every handler. Three canonical shapes (full
reference in [Expressions](../poco/04-expressions.md)):

| Shape | When |
|---|---|
| `#[computed]` method | a pure function of other fields |
| `#[watch(field)]` method | needs `self` / has side effects; recompute on a field change |
| plain handler | the value only changes through user action |

```rust
#[handlers]
impl Counter {
    // Recomputed whenever `count` changes; bound as pp-text="count_label".
    #[computed]
    pub fn count_label(count: i32) -> String {
        format!("{count} item{}", if count == 1 { "" } else { "s" })
    }
}
```

`#[computed]` methods are **static** (no `self`) and take the fields they
depend on as parameters; `#[watch(field)]` methods take `(new: V, prev:
Option<V>)` and run when the field changes.

## How effects and the proxy connect

Every component scope exposes a `js_sys::Proxy`. When a directive
evaluates an expression, it reads fields through that proxy. Each proxy
`get` trap calls `track(scope_id, key)`, recording the current effect as a
subscriber of that `(scope, field)` pair. Each proxy `set` trap calls
`trigger(scope_id, key)`, queuing the field's subscribers for the next
microtask flush.

```text
directive body runs inside an effect
  └─ CURRENT_EFFECT = Some(id)
     └─ proxy.get("count")  ← get trap fires
        └─ track(scope_id, "count")
           ├─ DEPS[scope_id]["count"].insert(effect_id)
           └─ REVERSE[effect_id].insert((scope_id, "count"))
```

When the flush runs, each effect clears its previous dependency set before
re-executing, so conditional reads (`if a { b } else { c }`) never leave
stale subscriptions behind.

### Handler mutations

`#[handlers]` methods mutate `&mut self` directly, bypassing the proxy.
After the method returns, the runtime calls `trigger_scope(scope_id)`,
which fans out to every currently-tracked key in that scope. Effects that
subscribed to any of those keys requeue for the next flush — coarser than
a single-field trigger, but correct (see [Known limits](#known-limits)).

## Lower-level primitives

Most components never touch these — field mutation plus `#[computed]` /
`#[watch]` cover the common cases. They are available for standalone
reactive work: an `effect` that reads a `#[store]`, library code, a custom
directive.

| Primitive | What it does |
|---|---|
| `effect(f)` | Runs `f` immediately; reruns on any dep change. |
| `effect_scoped(f)` | Like `effect`, but released automatically on unmount. |
| `computed(f)` | Memoized derivation — runs lazily, only when a dep changed since the last read. |
| `watch(source, cb)` | Calls `cb(&new, prev: Option<&T>)` on the first run and whenever `source()` changes. |
| `watch_field("field", cb)` | Reactive watcher on a single named field of the current scope. |
| `on_cleanup(f)` | Teardown to run before an effect reruns or is released. |
| `batch(f)` | Coalesces multiple triggers inside `f` into one flush. |

### Effect cleanup

`on_cleanup` runs before each rerun and on final `release`, so timers,
subscriptions, and listeners are always torn down correctly.

```rust
effect_scoped(|| {
    let handle = timers::every(1000, || tracing::info!(target: "pocopine.log", "tick"));
    on_cleanup(move || handle.cancel());
});
```

### Batching

Wrap multiple writes in `batch` to merge them into one flush.

```rust
let cart = store::<Cart>();
batch(|| {
    cart.update(|c| c.add(item_a));
    cart.update(|c| c.add(item_b));
});
// one flush, not two
```

`batch` is nestable; the flush fires only when the outermost batch exits
and there are queued effects.

## Known limits

- **Reactivity is per-field, by name.** No nested-field tracking, no
  per-index array tracking, no collection-mutation tracking. Writing
  `self.user.name` does not invalidate a subscriber of `self.user`.
- **Handler mutations trigger every tracked key in scope.** Fine-grained
  per-field handler triggers are not implemented yet; a handler that
  touches one field reruns every effect that tracked any field in the
  scope.
- **Cross-component state goes through `#[store]`**, prop passing, or
  `provide`/`inject` — there is no other shared-state channel.

## Further reading

- [`01-current-design.md`](./01-current-design.md) — internals: the
  thread-local tables, the proxy trap lifecycle, and the microtask flush.
- [`02-roadmap.md`](./02-roadmap.md) — what's shipped and what's next,
  ordered by leverage.
- [`04-vue3-reference.md`](./04-vue3-reference.md) — how Vue 3's
  `reactive` / `effect` / `computed` primitives map onto pocopine's.
