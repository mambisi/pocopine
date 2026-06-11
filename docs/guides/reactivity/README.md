---
title: "Reactivity overview"
description: "How pocopine connects Rust component state to the DOM with no virtual DOM and no render calls — and where to go next."
---

# Reactivity

Pocopine connects your Rust component state to the DOM with **no virtual
DOM and no render calls**. Every field of a `#[component]` struct is
backed by an interned **signal**. A directive that *reads* a field
**subscribes** to that signal; a write **re-runs** every subscribed
directive on the next microtask. There is no `useState` and no cell to
wrap your values in — a plain `pub` struct field *is* the reactive
primitive, and the tracking happens Rust-side (no JS proxy on the hot
path). The ergonomics are the ones Vue 3 calls `reactive()`; the engine
underneath is fine-grained signals, as in Solid or Leptos.

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

This page is the map. The mental model fits in a paragraph; the rest of
the section drills down by audience. Use [What to reach for](#what-to-reach-for)
to jump straight to the page you need.

## The mental model

Four rules cover everything:

- **Read = subscribe.** Evaluating a directive calls
  `track(scope_id, key)`, which interns a signal for that
  `(scope, field)` pair and records the directive's effect as its
  subscriber.
- **Write = re-run.** Writing a field calls `trigger(scope_id, key)`,
  queuing the signal's subscribers for the next microtask flush.
- **Per-field, by name.** Subscriptions key on the field *name* in a scope
  — `"count"` — not on a value, an index, or a nested path.
- **Handlers are fingerprint-swept.** A `#[handlers]` method mutates
  `&mut self` directly. The runtime hashes every observed field before the
  call and again after, then triggers **only the fields whose hash moved**
  — a handler that touches one field out of twenty re-runs one field's
  effects.

```text
@click="increment"
  └─ DirtySweep::begin        hash every observed field (Rust-side)
     └─ Counter::increment(&mut self)   self.count += 1
        └─ DirtySweep::finish  re-hash → only `count` changed
           └─ trigger(scope_id, "count")
              └─ effect for pp-text="count" requeued
                 └─ microtask flush → re-read count → DOM patched
```

## What to reach for

Pick by what owns the state or what you are trying to do, then follow the
link down.

| I want to… | Reach for | Page |
|---|---|---|
| Hold state owned by one component | a plain `pub` struct field | [Essentials](./01-essentials.md) |
| Pass state parent → child | `pp-bind:` attribute + `#[prop]` field | [Essentials](./01-essentials.md) |
| Report an event child → parent | `emit(name, detail)` + `@event` listener | [Essentials](./01-essentials.md) |
| Share state app-wide | `#[store]` (read `$store.<name>`, write `store::<T>()`) | [Essentials](./01-essentials.md) |
| Derive display state | `#[computed]` (static, deps-as-params) | [Essentials](./01-essentials.md) |
| React to a field changing | `#[watch(field)]` `(new, prev)` | [Essentials](./01-essentials.md) |
| Update from Rust (async, lifecycle) | `dispatch!` / `this::<T>()` / `Handle::update` | [Essentials](./01-essentials.md) |
| Run a standalone effect / timer / schedule work | `effect` · `timers` · `tick` · `batch` | [Utilities](./02-utilities.md) |
| Keep standalone reactive state (rare) | `signal` / `rw_signal` (escape hatch) | [Utilities](./02-utilities.md) |
| Extend the runtime / understand the internals | the signal graph, flush, computed engine | [Internals](./03-internals.md) |
| Map a Vue 3 habit across | `reactive` / `computed` / `watch` / `ref` | [Vue 3 reference](./04-vue3-reference.md) |

For the everyday cookbook with one worked example per tool, start at
[Essentials](./01-essentials.md). For the four-category state decision
table, see [State management](../components/02-state.md); for what you can
write inside `pp-*="..."`, see [Expressions](../poco/04-expressions.md).

## Known limits

- **Reactivity is per-field, by name.** No nested-field tracking
  (`self.user.name`), no per-index or collection-mutation tracking —
  though the dirty sweep means mutating `self.rows[3]` in a handler still
  triggers only `rows`, not every field in the scope.
- **Cross-component state goes through `#[store]`**, props, or
  `provide`/`inject` — there is no other shared-state channel.

The mechanics behind these live in [Internals](./03-internals.md); what's
planned to lift them is in [Roadmap](./05-roadmap.md).

## Further reading

- [Essentials](./01-essentials.md) — the everyday cookbook: fields,
  `#[computed]`, `#[watch]`, `#[store]`, handles, and async `dispatch!`.
- [Utilities](./02-utilities.md) — the standalone toolbox: `effect`,
  `batch`, `computed()`, watchers, `tick`, `timers`, `refs`, context, and
  the signals escape hatch.
- [Internals](./03-internals.md) — the signal graph, the read/write
  lifecycle, the handler dirty sweep, flushing, batching, and the
  computed engine.
- [Vue 3 reference](./04-vue3-reference.md) — convergence and divergence
  with Vue 3's reactivity.
- [Roadmap](./05-roadmap.md) — what's shipped and what's next.
- [API reference](./06-api-reference.md) — generated signatures for the
  full reactive surface.
