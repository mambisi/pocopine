---
title: "Component structure"
description: "Opinionated defaults, with Rust's module conventions intact. A .rs file is a Rust module and can hold multiple components, helper types, and free functions…"
---

# Component structure

Opinionated defaults, with Rust's module conventions intact. A `.rs`
file is a Rust module and can hold multiple components, helper types,
and free functions — same as any Rust file. What *is* strictly
per-component is the template and (optional) stylesheet: one `.poco`
per component, one `.css` per component.

## File layout

Components live under `src/components/`. Rust modules group related
components; templates and stylesheets sit next to the `.rs` that
declares them.

```
src/
  components/
    counter.rs          # module — declares `Counter`
    Counter.poco         # template for Counter
    Counter.css         # styles for Counter

    todo.rs             # module — declares `TodoList` and `TodoItem`
    TodoList.poco        # template for TodoList
    TodoList.css        # styles for TodoList
    TodoItem.poco        # template for TodoItem
    TodoItem.css        # styles for TodoItem
  lib.rs
```

**Rules:**

* **Group by feature in one `.rs` module** when the components are
  used together (`TodoList` + `TodoItem` → `todo.rs`). Split into
  separate `.rs` files when they're used independently (`NavBar`,
  `Footer` → `nav_bar.rs`, `footer.rs`).
* **Don't create per-component directories** (`components/Counter/...`)
  until a component accumulates enough state to warrant its own
  module tree. Start flat; promote later.
* **Template names are `PascalCase`**, one per component. The macro
  defaults `template = "<StructName>.poco"` resolved next to the `.rs`
  file — no explicit path unless overriding.
* **Stylesheet names are your choice**, but one `.css` per component.
  The stylesheet is **not** auto-discovered; you must pass
  `style = "..."` explicitly (e.g. `#[component(style = "Counter.css")]`).
* **Runtime name is auto-derived** kebab-case of the struct ident
  (`Counter` → `counter`, `TodoItem` → `todo-item`). Pass
  `name = "..."` only to override.

`lib.rs` declares the module tree and wires components with `App`:

```rust
mod components {
    pub mod counter;
    pub mod todo;
}

use pocopine::prelude::*;

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<components::counter::Counter>()
        .register::<components::todo::TodoList>()
        .register::<components::todo::TodoItem>()
        .run();
}
```

## Shape of a component module (`.rs`)

A module may declare one component or several. Each component follows
the same shape.

### Single-component module

```rust
// components/counter.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub count: i32,
    pub loading: bool,
}

#[handlers]
impl Counter {
    pub fn on_mount(&mut self) { self.count = 0; }
    pub fn increment(&mut self) { self.count += 1; }
    pub fn reset(&mut self)     { self.count = 0; }
}
```

### Multi-component module (related components + helpers)

```rust
// components/todo.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

// ---------- helper types ----------

#[derive(Clone, Serialize, Deserialize)]
pub struct Todo {
    pub id: u32,
    pub label: String,
    pub done: bool,
}

// ---------- TodoList ----------

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoList {
    pub todos: Vec<Todo>,
    pub draft: String,
}

#[handlers]
impl TodoList {
    pub fn on_mount(&mut self) { self.todos.clear(); }
    pub fn add(&mut self)      { /* ... */ }
    pub fn clear(&mut self)    { self.todos.clear(); }
}

// ---------- TodoItem ----------

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoItem {
    pub id: u32,
    pub label: String,
    pub done: bool,
}

#[handlers]
impl TodoItem {
    pub fn toggle(&mut self) { self.done = !self.done; }
}
```

**Ordering convention inside a module:**

1. `use` statements
2. Module-local helper types (plain structs/enums, not components)
3. Each component, as a triplet: `pub struct ... { ... }`,
   `#[handlers] impl ... { ... }`, a comment-separator banner between
   components if there's more than one.

### Handler conventions (apply per component)

* **One `#[handlers] impl` per component.** Don't split across blocks.
* **Lifecycle first, actions second, async last**, inside the block.
* **Lifecycle hooks are named methods**: `on_setup` runs before the
  template walk (inject context, compute initial state), `on_mount`
  runs after the subtree is fully bound, `on_ready` runs one microtask
  later (takes `&self`), `on_unmount` runs on teardown. Omit any hooks
  you don't need.
* **Action names are bare verbs** (`increment`, `save`, `toggle`,
  `load`). No `on_` prefix, no `handle_` prefix — the template makes
  intent obvious (`pp-on:click="increment"`).
* **Handlers take `&mut self`** optionally followed by typed event or
  value arguments. Each typed arg must implement `FromHandlerArg`; the
  common web event types (`InputEvent`, `KeyboardEvent`, `MouseEvent`,
  etc.) implement it out of the box.

### State field conventions

* **All reactive state is `pub`.** Template access goes through the
  proxy; private fields aren't reachable from templates. If a field is
  genuinely private (e.g., a cache handle), it lives as local state
  inside a handler, not on the struct.
* **`Default + Serialize + Deserialize` are always derived.** The
  macro needs them for instantiation and the proxy `get`/`set` path.
* **Non-Serialize handles** (e.g., JS objects, tokio channels in
  native builds) don't belong on the struct. Stash them in a
  `thread_local` keyed by scope id, or promote the need to a store.

## Shape of a template (`.poco`)

```poco
<div class="counter">
  <button pp-on:click="decrement">-</button>
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

**Rules:**

* **Single root element.** The macro auto-stamps the scope marker on
  it at build time; you never write `pp-data` or `pp-init` in your
  templates.
* **Top-down attribute order**: structural directives first
  (`pp-if`, `pp-for`, `pp-show`), then presentational (`class`, `id`),
  then reactive bindings and event listeners. Readable at a glance;
  don't interleave.
* **Attribute values are expressions** — a field name (`pp-text="count"`),
  a handler name (`pp-on:click="increment"`), or a compound expression
  (`pp-show="!loading && applied_query"`).

## Shape of a stylesheet (`.css`)

```css
.wrapper { display: flex; gap: 0.5rem; }
.count   { font-size: 2rem; font-weight: 600; }
button   { padding: 0.5rem 1rem; }
```

**Rules:**

* **Scoped by default** (see `../poco/03-scoped-styles.md`).
* **Style by class** where practical — scoped element selectors work
  but classes are grep-able and survive refactors better.
* **Opt out of scoping** per-rule with `:global(...)`. Don't fight
  scoping with `!important`.
* **Pass the path explicitly**: `#[component(style = "Counter.css")]`.
  The macro does not auto-discover the stylesheet.

## What the macro infers so you don't have to

Given a component `pub struct TodoItem { ... }` declared in
`components/todo.rs`:

| Field | Default |
|---|---|
| `name` | kebab-case of ident → `"todo-item"` |
| `template` | `"TodoItem.poco"` (next to `todo.rs`) |
| `style` | none — must be passed as `style = "..."` if needed |

So the minimal component is just:

```rust
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter { pub count: i32 }

#[handlers]
impl Counter {
    pub fn increment(&mut self) { self.count += 1; }
}
```

Explicit `name` / `template` / `style` arguments are for overrides
only — e.g., when two components need to share a template, or when
the stylesheet lives in a shared file.

## Non-choices (don't ask)

* **No `React.memo` / `shouldUpdate`** equivalent. The reactive engine
  reruns only the effects whose deps changed.
* **No functional components.** Every component is a struct + handlers.
* **No "render" function.** The template file is the render.
* **No `useEffect` / `useMemo` hooks.** Reactive work happens in
  directives and `effect` / `computed` primitives — not as hooks.
* **No higher-order components / mixins.** Composition is via child
  components with their own scope (see `02-state.md`).
