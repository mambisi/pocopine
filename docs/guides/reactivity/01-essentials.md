---
title: "Reactivity essentials"
description: "The everyday reactive toolkit: fields, #[computed], #[watch], #[store], handles, and async actions."
---

# Reactivity essentials

This is the day-to-day cookbook. One real, minimal example per tool. For
the mental model see the [Overview](./README.md); for standalone effects,
timers, and scheduling see [Utilities](./02-utilities.md).

The whole system reduces to two rules: **a directive that reads a field
subscribes; a handler that writes a field re-runs every subscriber on the
next microtask.** Everything below is sugar over that loop.

## Field reactivity

A plain `pub` field **is** the reactive primitive — no `useState`, no cell
to wrap values in. A directive that reads it subscribes; a `&mut self`
handler that writes it re-runs the subscriber.

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

`pp-text="count"` reads `count` → subscribes. `increment` writes it →
the `<span>` re-runs. `:attr` bindings (`:src`, `:class`, …) subscribe the
same way.

For input, `pp-model` is two-way: it writes the bound field on every input
event and reflects field changes back into the control. On your own
fields it works out of the box:

```poco
<input pp-model="current_label" />
```

To make a field two-way bindable **from a parent** (`pp-model:field` on
your component tag), mark it `#[model]`. The component emits the
per-field update channel automatically:

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineToggle.poco", role = "interactive")]
pub struct PineToggle {
    /// Two-way bindable via `pp-model:pressed="bool"`.
    #[model]
    pub pressed: bool,
}

#[handlers]
impl PineToggle {
    pub fn toggle(&mut self) { self.pressed = !self.pressed; }
}
```

A parent then binds it:

```poco
<pine-toggle pp-model:pressed="bold"><strong>B</strong></pine-toggle>
```

See [State management](../components/02-state.md) for the full
field-vs-prop-vs-store decision table and [Expressions](../poco/04-expressions.md)
for what reads are legal in a template.

## Deriving values — #[computed]

When a value is a pure function of other fields, don't recompute it by
hand in every handler — declare it once. A `#[computed]` method is
**static** (no `self`), takes the fields it depends on as parameters, and
is exposed as a read-only synthetic field of the same name.

```rust
#[handlers]
impl FullName {
    #[computed]
    pub fn full(first: String, last: String) -> String {
        format!("{first} {last}")
    }
}
```

```poco
<p pp-text="full"></p>
```

`full` recomputes only when `first` or `last` change — and only when
something actually reads it (it is lazy and memoized; repeat reads return
the cache). A `#[computed]` may depend on another `#[computed]` by value;
the macro orders the graph for you. Cycles are a compile error.

The free function `computed()` behind this macro — for standalone derived
values not tied to a component — lives in [Utilities](./02-utilities.md#memoized-derivations-computed).

## Reacting to changes — #[watch(field)]

When a change needs `self` or has a side effect (mirror into another
field, kick off an animation, reset a buffer), use `#[watch(field)]`. The
method takes `(new, prev)` where `prev` is `None` on the first call after
mount, and runs whenever `field` changes.

```rust
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PinCardDemo.poco")]
pub struct PinCardDemo {
    pub card_number: String,
    pub pin: String,
    /// Derived mirror, kept as a plain bool so the template can
    /// `pp-show="complete"`.
    pub complete: bool,
}

#[handlers]
impl PinCardDemo {
    #[watch(card_number)]
    fn on_card_change(&mut self, _new: String, _prev: Option<String>) {
        self.recompute_complete();
    }

    #[watch(pin)]
    fn on_pin_change(&mut self, _new: String, _prev: Option<String>) {
        self.recompute_complete();
    }
}

impl PinCardDemo {
    fn recompute_complete(&mut self) {
        self.complete =
            self.card_number.chars().count() == 16 && self.pin.chars().count() == 4;
    }
}
```

Reach for `#[computed]` when the result is a pure projection you read in
the template; reach for `#[watch(field)]` when you need `&mut self` or a
side effect. The free-function family (`watch` / `watch_field`) is in
[Utilities](./02-utilities.md#watchers-watch-watch-field-scoped).

## App-wide state — #[store]

Cross-component state lives in a `#[store]`: a singleton scope that
outlives any DOM mount. One instance per type per runtime. An empty
`#[handlers] impl` is required, same as a component.

```rust
#[derive(Serialize, Deserialize)]
#[store]
pub struct Preferences {
    pub theme: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self { theme: "light".into() }
    }
}

#[handlers]
impl Preferences {}
```

Register it on the app and use it two ways.

**In templates** — the `$store.<name>` magic resolves through the store's
scope, so reads participate in normal dep tracking:

```poco
<input pp-model="$store.preferences.theme" />
<strong pp-text="$store.preferences.theme"></strong>
```

**In Rust** — `store::<T>()` returns a `Handle<T>`. `with` reads a
snapshot (non-reactive); `update` mutates and notifies subscribers when
the closure returns:

```rust
if store::<Preferences>().with(|p| p.theme == "dark") {
    store::<Preferences>().update(|p| p.theme = "light".into());
}
```

The store name defaults to the kebab-case of the struct ident; override
with `#[store(name = "...")]`.

## Driving updates from Rust — Handle & this()

Inside a handler, `this::<Self>()` returns a `Handle<Self>` — the same
typed handle `store::<T>()` returns. Use it when state must change from
**outside** the normal handler return: an async callback, a timer, a DOM
listener, a lifecycle hook.

- `handle.update(|s| …)` — mutate `&mut s`, then notify subscribers. It
  also binds the scope for the duration of the closure, so `this()` /
  `dispatch!` work inside even from a detached async task.
- `handle.with(|s| …)` — non-reactive snapshot read.

A scope-bound timer pushing live updates back into the component:

```rust
pub fn on_setup(&mut self) {
    let handle = this::<Self>();
    pocopine::timers::every_scoped(3200, move || {
        handle.update(|c| c.syncing = true);
        let h = handle.clone();
        pocopine::timers::after_scoped(900, move || h.update(|c| c.syncing = false));
    });
}
```

`*_scoped` timers cancel when the component unmounts, so the handle never
fires into a dead scope.

### Gotcha: don't call update synchronously in on_ready

`on_ready` runs with `&self` behind an **immutable** borrow of the
scope's state. A synchronous `handle.update(…)` there needs a mutable
borrow of the same `RefCell` and panics with a double-borrow. Defer the
write one microtask with `tick::next` so the lifecycle frame unwinds
first:

```rust
pub fn on_ready(&self, handle: Handle<Self>) {
    pocopine::tick::next(move || {
        handle.update(|s| s.ready = true);
    });
}
```

The same rule is why `watch_field` and friends defer their install
internally — see [Utilities](./02-utilities.md#scheduling-work-tick).

## Async actions — dispatch! and spawn_scoped

The canonical async-handler shape is "do `X`, then apply the result to
`self`." `dispatch!` captures `this::<Self>()`, spawns the body on the
microtask executor, and routes the awaited value through `Handle::update`
so reactivity fires exactly once at the end:

```rust
#[handlers]
impl BlogPost {
    pub fn on_mount(&mut self) {
        self.loading = true;
        let post_id = self.post_id;
        dispatch!(get_post(post_id).await, |s, result| {
            s.loading = false;
            match result {
                Ok(p)  => { s.title = p.title; s.body = p.body; s.error.clear(); }
                Err(e) => { s.error = e.to_string(); }
            }
        });
    }
}
```

`s` is `&mut Self`, `result` is the awaited value. Set the loading flag
*before* `dispatch!` (synchronously, in the handler) so the spinner shows
on the current flush; the result block runs later.

For an open-ended async loop — polling, an animation driver, a long-lived
subscription — reach for `spawn_scoped`, which ties the task to the
scope's lifetime (it is cancelled at unmount) and hands you the
`Handle<Self>` to write through:

```rust
fn start_slice_animation_loop(&mut self) {
    let me = this::<Self>();
    pocopine::spawn_scoped(async move {
        loop {
            let now = pocopine::timers::next_frame().await;
            if me.update(|chart| chart.tick_slice_animations(now)) {
                break; // returns true when the animation is done
            }
        }
    });
}
```

`dispatch!` is the right tool for one-shot "fetch then update"; reach for
`spawn_scoped` (or `spawn_latest` for latest-wins, e.g. type-ahead
search) when you own the loop. See [Utilities](./02-utilities.md#timers-timers)
for the timer and scheduling primitives these build on.

## Cheat sheet

| Intent | Tool | Import |
|---|---|---|
| Hold state owned by a component | a `pub` struct field | — |
| Two-way bind your own input | `pp-model="field"` | template directive |
| Accept two-way binding from a parent | `#[model]` field | `pocopine::prelude::*` |
| Derive a value from other fields | `#[computed]` method | `pocopine::prelude::*` |
| Run on a field change (with side effect) | `#[watch(field)]` method | `pocopine::prelude::*` |
| Share state app-wide | `#[store]` struct | `pocopine::prelude::*` |
| Read a store in a template | `$store.<name>.<field>` | template magic |
| Get a handle to a store | `store::<T>()` | `pocopine::prelude::*` |
| Get a handle to the current component | `this::<T>()` | `pocopine::prelude::*` |
| Mutate + notify from Rust | `Handle::update(\|s\| …)` | (via the handle) |
| Snapshot read from Rust | `Handle::with(\|s\| …)` | (via the handle) |
| Fetch then update self | `dispatch!(expr.await, \|s, r\| …)` | `pocopine::prelude::*` |
| Own an async loop tied to the scope | `spawn_scoped(fut)` | `pocopine::prelude::*` |
| Defer a write off the current borrow | `tick::next(\|\| …)` | `pocopine::tick` |

See also: [Utilities](./02-utilities.md) for effects, batching, timers,
context, and the free-function forms · [State management](../components/02-state.md)
for choosing where state lives · [Expressions](../poco/04-expressions.md)
for template read syntax.
