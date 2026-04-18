# State management

Four categories of state, one canonical pattern each. Pick by
answering a single question.

| Question | Pattern |
|---|---|
| Does only this component touch it? | **Local state** → struct field |
| Does a child need to react to a parent? | **Parent → child** → nested `pp-data`, child reads parent's proxy via chain walk |
| Does a parent need to know something happened in the child? | **Child → parent** → `$dispatch` event, parent handles via `pp-on:event` |
| Does anything outside this subtree need it? | **Global store** → `#[store]`, accessed via `$store.name` |

If none of the four fits, the state probably doesn't belong where
you're trying to put it. Check again before inventing a fifth pattern.

---

## 1. Local state

The default. State lives in struct fields.

```rust
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub count: i32,
    pub loading: bool,
}

#[handlers]
impl Counter {
    pub fn increment(&mut self) { self.count += 1; }
}
```

Template reads fields by name:

```html
<span pp-text="count"></span>
<button pp-show="!loading" pp-on:click="increment">+</button>
```

**Do:** keep everything the component owns here. No ceremony.

**Don't:** reach for a store because the field *might* one day be
shared. Promote when the second consumer actually appears — not before.

---

## 2. Parent → child

Parents mount children as kebab-case tags named after the child
struct. Attributes on the tag flow into the child's state. Static
attrs seed once; `pp-bind:` attrs stay reactive. Full specification
in `03-composition.md`.

```html
<!-- TodoList.pcx -->
<div pp-init="init">
  <h1 pp-text="title"></h1>
  <todo-item id="1" label="Buy milk" />
  <todo-item pp-bind:id="current.id" pp-bind:label="current.label" />
</div>
```

```rust
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoItem {
    pub id: u32,      // <- fills from `id="..."` or `pp-bind:id="..."`
    pub label: String, // <- fills from `label="..."` or `pp-bind:label="..."`
    pub done: bool,
}
```

**Do:** expose every parent-configurable piece of state as a `pub`
field on the child struct. Static attrs for constants, `pp-bind:` for
anything reactive.

**Don't:** reach across components by struct-accessing the parent.
Scopes are isolated by design — the prop surface is the attribute
list, nothing else.

---

## 3. Child → parent

Children signal upward by dispatching a `CustomEvent`. The parent
listens on the DOM with `pp-on:event-name`.

```rust
// TodoItem.rs
#[handlers]
impl TodoItem {
    pub fn complete(&mut self) {
        self.done = true;
        // $dispatch is a magic; wire available in next milestone.
        // For now: use invoke-and-dispatch in a handler (sketch).
    }
}
```

```html
<!-- TodoItem.pcx -->
<li pp-data="todo-item">
  <button pp-on:click="complete">✓</button>
</li>

<!-- TodoList.pcx -->
<ul pp-data="todo-list" pp-on:todo-completed="record_completed">
  <li pp-data="todo-item">...</li>
</ul>
```

**Do:** use kebab-case event names (`todo-completed`, `item-removed`).
**Do:** carry a `detail` payload with only the info the parent needs —
not a full state snapshot.

**Don't:** store a reference to the parent inside the child. The
parent finds the child through the DOM; the child talks back through
the DOM. No other coupling.

**Don't:** invent custom pub/sub channels. Events bubble; that's the
channel.

---

## 4. Global state — stores

For anything that lives longer than a subtree: a shopping cart, a
current user, a theme. Stores are singletons registered at startup.

Sketch of the planned surface (not yet implemented):

```rust
// src/stores/cart.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[store]  // name = "cart" (kebab-case of struct ident)
pub struct Cart {
    pub items: Vec<Item>,
}

#[handlers]
impl Cart {
    pub fn add(&mut self, item: Item) { self.items.push(item); }
    pub fn clear(&mut self)           { self.items.clear(); }
}
```

Registered once at startup:

```rust
#[wasm_bindgen(start)]
fn main() {
    Cart::register();
    // ... components ...
    pocopine::run();
}
```

Read from any template via the `$store` magic:

```html
<div pp-data="nav-bar">
  Items: <span pp-text="$store.cart.items.length"></span>
</div>
```

Call store handlers from a component's handler:

```rust
#[handlers]
impl AddButton {
    pub fn add(&mut self) {
        store::<Cart>().add(Item { ... });
    }
}
```

**Do:** name stores after the *thing* they represent (`cart`, `session`,
`theme`), not the *action* (`cart-manager`).
**Do:** keep each store focused on one domain.

**Don't:** make every component talk to a store. Most components
should be local-state only. A store is for genuinely shared state.

**Don't:** create "global singletons" by hand (`static CART: OnceCell<...>`).
Use `#[store]` so reactivity just works.

> Stores are designed but not yet implemented. When we ship them, the
> above surface is the contract. Until then, if you need shared state,
> write a component scope that wraps the subtree — and flag the
> promotion to a store in your PR.

---

## Async data / server functions

Planned surface for the server-functions milestone. Written here so
component authors know the canonical pattern ahead of time.

```rust
// src/server/todos.rs
#[server]
pub async fn list_todos() -> Result<Vec<Todo>, ServerError> {
    // Runs on the server; client gets a typed REST binding.
    db::todos::list().await
}
```

```rust
// src/components/TodoList.rs
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoList {
    pub todos: Vec<Todo>,
    pub loading: bool,
    pub error: Option<String>,
}

#[handlers]
impl TodoList {
    pub async fn init(&mut self) {
        self.loading = true;
        match list_todos().await {
            Ok(todos) => self.todos = todos,
            Err(e)    => self.error = Some(e.to_string()),
        }
        self.loading = false;
    }
}
```

**Do:** store the request's tri-state (`data | loading | error`) as
three sibling fields on the struct. That's the opinionated shape; no
`Result` enum exposed to templates.
**Do:** put fetching in `init` for "load once" and in a named handler
(`refresh`, `reload`) for "load on demand."

**Don't:** fetch inside a template expression.
**Don't:** store a `Future`, `Promise`, or an in-flight request handle
on the struct. If you need to cancel, model it with a flag the handler
checks — not by holding the handle.

---

## Anti-patterns

A deliberately short list, because an opinionated framework shouldn't
need a long one:

1. **Global mutable state outside a store.** Any `static mut`,
   `OnceCell<RefCell<...>>`, or similar, used for app state. Promote
   to a store or move it into a component scope.
2. **Component fields that aren't serializable.** They won't round-trip
   through the proxy's get/set. If you need a non-Serialize handle
   (e.g., a JS object), stash it in a separate `thread_local` keyed by
   `ScopeId` — but the *public, reactive* fields must be Serialize.
3. **Talking to another component's scope from outside.** Scopes are
   addressed by `ScopeId` through the walker; user code doesn't get
   scope ids. If you're tempted, promote to a store.
4. **Derived state stored as a field you manually keep in sync.**
   Today: compute it inside the handler that changed the inputs, or in
   the directive. After signals land: `computed()`. Never two fields
   with an "update both" invariant.
