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
* **Template and stylesheet names are `PascalCase`**, one per
  component. The macro defaults `template = "<StructName>.poco"` and
  `style = "<StructName>.css"` resolved next to the `.rs` file — no
  explicit paths unless overriding.
* **Runtime name is auto-derived** kebab-case of the struct ident
  (`Counter` → `counter`, `TodoItem` → `todo-item`). Pass
  `name = "..."` only to override.

`lib.rs` declares the module tree and calls each `register`:

```rust
mod components {
    pub mod counter;
    pub mod todo;
}

use pocopine::prelude::*;

#[wasm_bindgen(start)]
fn main() {
    components::counter::Counter::register();
    components::todo::TodoList::register();
    components::todo::TodoItem::register();
    pocopine::run();
}
```

(If the register list grows beyond ~20, a `register_all!` helper
macro is the next thing we'll ship. Not before.)

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
    pub fn init(&mut self)      { self.count = 0; }
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
    pub fn init(&mut self)   { self.todos.clear(); }
    pub fn add(&mut self)    { /* ... */ }
    pub fn clear(&mut self)  { self.todos.clear(); }
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
* **Handler names are bare verbs** (`increment`, `save`, `toggle`,
  `load`). No `on_` prefix, no `handle_` prefix — the template makes
  intent obvious (`pp-on:click="increment"`).
* **Handlers take `&mut self` and nothing else** in the current
  milestone. Event objects and parameters land in a later milestone;
  until then, read what you need from other fields.

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

```html
<div pp-data="counter" pp-init="init">
  <button pp-on:click="decrement">-</button>
  <span class="count" pp-text="count"></span>
  <button pp-on:click="increment">+</button>
</div>
```

**Rules:**

* **Single root element** with `pp-data="<name>"` matching the
  component's runtime name.
* **Top-down attribute order**: `pp-data`, then `pp-init`, then
  presentational (`class`, `id`), then other directives. Readable at a
  glance; don't interleave.
* **Attribute values are bare identifiers** today — a field
  (`pp-text="count"`) or a handler (`pp-on:click="increment"`). Full
  Rust expressions in values are a future milestone.

## Shape of a stylesheet (`.css`)

```css
.wrapper { display: flex; gap: 0.5rem; }
.count   { font-size: 2rem; font-weight: 600; }
button   { padding: 0.5rem 1rem; }
```

**Rules:**

* **Scoped by default** (see `docs/poco/03-scoped-styles.md`).
* **Style by class** where practical — scoped element selectors work
  but classes are grep-able and survive refactors better.
* **Opt out of scoping** per-rule with `:global(...)`. Don't fight
  scoping with `!important`.

## What the macro infers so you don't have to

Given a component `pub struct TodoItem { ... }` declared in
`components/todo.rs`:

| Field | Default |
|---|---|
| `name` | kebab-case of ident → `"todo-item"` |
| `template` | `"TodoItem.poco"` (next to `todo.rs`) |
| `style` | `"TodoItem.css"` (next to `todo.rs`) if it exists, else nothing |

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
  directives (today) and `effect` / `computed` primitives (after
  signals land) — not as hooks.
* **No higher-order components / mixins.** Composition is via child
  components with their own scope (see `02-state.md`).
