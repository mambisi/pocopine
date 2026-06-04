---
title: "State management"
description: "Four categories of state, one canonical pattern each. Pick by answering a single question."
---

# State management

Four categories of state, one canonical pattern each. Pick by
answering a single question.

| Question | Pattern |
|---|---|
| Does only this component touch it? | **Local state** → struct field |
| Does a child need to react to a parent? | **Parent → child** → `pp-bind:` attributes on the child tag |
| Does a parent need to know something happened in the child? | **Child → parent** → `emit(name, detail)` from Rust; parent listens with `@event-name` |
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

```poco
<span pp-text="count"></span>
<button pp-show="!loading" @click="increment">+</button>
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

```poco
<!-- TodoList.poco -->
<div>
  <h1 pp-text="title"></h1>
  <todo-item id="1" label="Buy milk" />
  <todo-item pp-bind:id="current.id" pp-bind:label="current.label" />
</div>
```

```rust
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct TodoItem {
    pub id: u32,       // fills from `id="..."` or `pp-bind:id="..."`
    pub label: String, // fills from `label="..."` or `pp-bind:label="..."`
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

Children signal upward by dispatching a `CustomEvent`. Call
`pocopine::emit(name, detail)` from a Rust handler; the event bubbles
up the DOM. The parent listens with `@event-name` (or `pp-on:event-name`)
on the child tag or any ancestor.

```rust
// TodoItem.rs
#[handlers]
impl TodoItem {
    pub fn complete(&mut self) {
        self.done = true;
        pocopine::emit("todo-completed", self.id);
    }
}
```

```poco
<!-- TodoItem.poco -->
<li>
  <button @click="complete">✓</button>
</li>
```

```poco
<!-- TodoList.poco -->
<ul @todo-completed="record_completed">
  <todo-item pp-bind:id="item.id" pp-bind:label="item.label" />
</ul>
```

```rust
// TodoList.rs
#[handlers]
impl TodoList {
    // A bare `@todo-completed="record_completed"` passes the event
    // ($event); read the emitted payload off `.detail()`.
    pub fn record_completed(&mut self, ev: web_sys::CustomEvent) {
        let id = ev.detail().as_f64().unwrap_or_default() as u32;
        // …
    }
}
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
current user, a theme. Stores are singletons registered once at
startup and shared across every component.

```rust
// src/stores/preferences.rs
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[store]  // name = "preferences" (kebab-case of struct ident)
pub struct Preferences {
    pub theme: String,
}

#[handlers]
impl Preferences {
    pub fn toggle_theme(&mut self) {
        self.theme = if self.theme == "dark" { "light" } else { "dark" }.into();
    }
}
```

Registered on the app builder:

```rust
#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<NavBar>()
        .store::<Preferences>()
        .run();
}
```

Read from any template via the `$store` dotted path:

```poco
<div>
  Theme: <span pp-text="$store.preferences.theme"></span>
  <input pp-model="$store.preferences.theme" />
</div>
```

Call from Rust via the typed handle:

```rust
#[handlers]
impl ThemeButton {
    pub fn flip(&mut self) {
        pocopine::store::<Preferences>().update(|p| {
            p.theme = "dark".into();
        });
    }
}
```

`update` triggers every effect subscribed to any of the store's
fields, exactly like a handler invocation on a component scope.
Reads via `store::<T>().with(|p| ...)` are non-reactive (Rust-side);
in templates, reads always go through the proxy and track deps.

**Do:** name stores after the *thing* they represent (`preferences`,
`session`, `cart`), not the action (`theme-toggler`).
**Do:** keep each store focused on one domain.

**Don't:** make every component talk to a store. Most components
should stay local-state only. A store is for genuinely shared state.

**Don't:** create "global singletons" by hand (`static CART:
OnceCell<...>`). Use `#[store]` so reactivity just works.

---

## Async data / server functions

Server functions live next to the component that consumes them and
return `ServerResult<T>`. The client stub is auto-generated by the
`#[server]` macro; on the server side the same item is the real body.

```rust
#[pocopine::server]
pub async fn get_post(id: u32) -> ServerResult<Post> {
    // Runs on the server. On wasm32 this body is replaced with a
    // client stub that POSTs to the generated server-function route.
    db::posts::by_id(id).await.map_err(|e| ServerError::App(e.to_string()))
}
```

Consume it from a component. Handlers can't cross `.await` with
`&mut self`, so the canonical pattern is the `dispatch!` macro: it
runs the async body, then applies a synchronous update closure to
the scope when the future resolves.

```rust
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct BlogPost {
    pub post_id: u32,
    pub title: String,
    pub body: String,
    pub loading: bool,
    pub error: String,
}

#[handlers]
impl BlogPost {
    pub fn on_setup(&mut self) {
        self.loading = true;
        let post_id = self.post_id;
        dispatch!(
            get_post(post_id).await,
            |s, result| {
                s.loading = false;
                match result {
                    Ok(p)  => { s.title = p.title; s.body = p.body; s.error.clear(); }
                    Err(e) => { s.error = e.to_string(); }
                }
            },
        );
    }
}
```

What `dispatch!` expands to: it calls `pocopine::this::<Self>()` for
a typed handle, `spawn_local`s the first expression, and routes the
awaited value through `Handle::update(|s| ...)` with the second
closure — so reactivity fires exactly once when the future resolves.

Key properties:

* **No `JsValue` in user code.** Mutations are plain Rust field
  assignments (`s.title = post.title`); the macro-generated
  `ComponentState` impl handles any JS boundary internally.
* **No `current_scope_id` plumbing.** `this::<Self>()` inside
  `dispatch!` picks up the id from the handler context automatically.
* **One trigger per completion.** Every subscriber of any field you
  touch in the closure re-runs on the next microtask — same batching
  as a synchronous handler.

For the lower-level building blocks (when `dispatch!` isn't the right
shape — e.g. dispatching into a store instead of `Self`, or
firing-and-forgetting), call `pocopine::this::<T>()` or
`pocopine::store::<T>()` and use `Handle::update` / `Handle::with`
directly.

### Why struct fields, not reactive cells?

Frameworks like React (`useState`) and Solid (signals) make you wrap
each piece of state in a cell. Pocopine doesn't: a `pub` struct field
*is* the reactive unit, tracked through the scope proxy. There is no
`.value`, no read/write split, no cell to construct.

That keeps async updates clean:

* `|s, result|` in `dispatch!` updates N fields in one closure — no
  per-field setter calls to thread through.
* Struct fields auto-serialize to templates (`pp-text="title"`); a cell
  would need a wrapper.
* The Rust types on the struct are exactly what you bind to — nothing
  added on top.

Struct + `dispatch!` is the one canonical async pattern. If you find
yourself wanting a free-floating reactive cell inside a `#[component]`,
promote the data to its own component or a `#[store]` instead.

**Do:** model the request's tri-state as `data | loading | error`
sibling fields. That's the opinionated shape; no `Result` enum
exposed to templates.
**Do:** put fetching in `on_setup` for "load once" and in a named handler
(`refresh`, `reload`) for "load on demand."

**Don't:** fetch inside a template expression.
**Don't:** store a `Future`, `Promise`, or an in-flight request handle
on the struct. If you need cancellation, model it with a flag the
handler checks — not by holding the handle.
**Don't:** hand-roll scope lookups with `Scope::find(id)` +
`state.borrow_mut().set("field", JsValue::from_str(...))`. Use
[`this`] and typed field assignment — that's the whole point of the
handle API.

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
   Don't write `progress_label` updates into every handler that
   touches `progress`. Use `#[computed]` for pure derivations and
   `#[watch(field)]` for side-effecting reactions. The framework
   keeps the derived value up to date for you. Templates bind by
   name (`pp-text="progress_label"`). See
   [`docs/guides/poco/04-expressions.md`](../poco/04-expressions.md) for
   the full pattern and the canonical examples.
