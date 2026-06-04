---
title: "Lifecycle"
description: "The four lifecycle hooks — on_setup, on_mount, on_ready, on_unmount — their order, their receivers, and the borrow rules that matter."
---

# Lifecycle

A component has four lifecycle hooks. All are optional; declare the ones
you need as methods in the `#[handlers] impl`. The framework calls them at
fixed points around the template walk:

```text
  construct (Default)
        │
        ▼
   on_setup(&mut self)        ── pre-walk: seed state, read props/loader
        │
   ── template is walked, DOM created, pp-ref's registered ──
        │
        ▼
   on_mount(&mut self)        ── DOM + refs exist; measure, install listeners
        │
   ── one microtask (after first paint) ──
        │
        ▼
   on_ready(&self)            ── safe to schedule deferred work; #[watch]s arm here
        │
        ⋯ component lives ⋯
        │
        ▼
   on_unmount(&mut self)      ── teardown (framework auto-removes tracked listeners)
```

| Hook | Receiver | Phase | Rendered DOM / refs | Use it for |
|---|---|---|---|---|
| `on_setup` | `&mut self` | Setup | **not yet** (host only) | seeding derived initial state; reading a route `Loader<T>` |
| `on_mount` | `&mut self` | Mount | available | measuring the DOM, installing listeners that need a `pp-ref` |
| `on_ready` | `&self` | Ready | available | work that must run after first paint; capturing a `Handle` for later |
| `on_unmount` | `&mut self` | Unmount | detaching (refs may be cleared) | custom teardown beyond auto-removed listeners |

Each hook can take [extractors](./05-extractors.md) — typed values the
framework projects from the lifecycle context (a `Handle<Self>`, `Refs`,
the rendered `El`, a `Parent<T>`, …). The sections below show the common
ones; the full surface is on the extractors page.

## `on_setup`

Runs once, before the template is walked. The rendered DOM doesn't exist
yet, so this is for **state**, not the DOM: seed fields that other fields
or the template derive from.

```rust
#[handlers]
impl Counter {
    pub fn on_setup(&mut self) {
        self.count = 10;
    }
}
```

`on_setup` is also where a routed component receives its loader data, via
the `Loader<T>` extractor — see [Route guards & loaders](../routing/route-guards-and-loaders.md):

```rust
pub fn on_setup(&mut self, data: Loader<DashboardData>) {
    let data = data.into_inner();
    self.stats = data.stats;
}
```

## `on_mount`

Runs after the template is walked: the rendered root and every
`pp-ref` exist. Take `&mut self` and any element-dependent extractor.

```rust
#[handlers]
impl Chart {
    pub fn on_mount(&mut self, refs: Refs) {
        if let Some(canvas) = refs.get_as::<web_sys::HtmlCanvasElement>("surface") {
            self.draw(&canvas);
        }
    }
}
```

## `on_ready`

Runs one microtask after `on_mount` — after the browser's first paint.
Its receiver is **`&self`**, not `&mut self` (see [Borrow rules](#borrow-rules)
below). It's the place to schedule deferred work and to capture a
`Handle<Self>` for use in a later callback.

```rust
#[handlers]
impl Menu {
    pub fn on_ready(&self, handle: Handle<Self>, refs: Refs) {
        // Resolve refs *now* (synchronously), capture them + the handle
        // into the listener closure that fires later.
        let trigger = refs.get("trigger");
        events::on_scoped(&trigger.unwrap(), events::ev::click, move |_| {
            handle.update(|s| s.open = !s.open);
        });
    }
}
```

`#[watch(field)]` methods are wired up here automatically — you don't call
them; the generated `on_ready` registers each watcher. See
[Expressions](../poco/04-expressions.md#2-watch-field-needs-self-recomputes-on-prop-change).

## `on_unmount`

Runs when the component is torn down. Listeners installed through the
framework (`@event` bindings, `events::on_scoped`, `timers::*_scoped`) are
removed automatically — use `on_unmount` only for teardown the framework
can't see (a hand-built `IntersectionObserver`, a `requestAnimationFrame`
loop you own).

```rust
#[handlers]
impl Sticky {
    pub fn on_unmount(&mut self) {
        self.observer.take();   // drop a manually-held observer
    }
}
```

## Borrow rules

Two rules cause nearly all lifecycle surprises. Both come from the same
fact: a hook borrows the component's state for the duration of the call.

### `on_ready` is `&self` — defer mutation

Because `on_ready` runs with an **immutable** borrow of the state, calling
`handle.update(...)` *synchronously* inside it re-enters the same `RefCell`
mutably and panics. Read and set up synchronously; push any mutation to the
next microtask:

```rust
pub fn on_ready(&self, handle: Handle<Self>) {
    // ✗ handle.update(|s| s.ready = true);   // panics: double borrow
    tick::next(move || {
        handle.update(|s| s.ready = true);    // ✓ borrow has cleared
    });
}
```

### Scope context ends with the hook — resolve refs synchronously

The element-dependent extractors (`Refs`, `El`, `TypedEl`) and
`refs::get_on` / `current_scope_id()` only resolve **during** the hook.
Inside a `tick::next` / `timers` / async callback they return `None` — the
scope context that backs them is gone. So resolve the DOM you need *up
front* and capture the concrete `Element` into the deferred closure:

```rust
pub fn on_ready(&self, refs: Refs, handle: Handle<Self>) {
    let panel = refs.get("panel");            // resolve NOW
    tick::next(move || {
        if let Some(panel) = panel {          // use the captured node
            let h = panel.get_bounding_client_rect().height();
            handle.update(move |s| s.panel_height = h);
        }
    });
}
```

## Related

- [Extractors](./05-extractors.md) — every typed value a hook can take.
- [State management](./02-state.md) — what to put in fields and stores.
- [Route guards & loaders](../routing/route-guards-and-loaders.md) — the
  `Loader<T>` that `on_setup` reads on a routed component.
