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
<!-- TodoList.poco -->
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
<!-- TodoItem.poco -->
<li pp-data="todo-item">
  <button pp-on:click="complete">✓</button>
</li>

<!-- TodoList.poco -->
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

// An empty `#[handlers]` is still required, same as for components.
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
fn main() {
    App::new()
        .register::<NavBar>()
        .store::<Preferences>()
        .run();
}
```

Read from any template via the `$store` dotted path:

```html
<div>
  Theme: <span pp-text="$store.preferences.theme"></span>
  <input pp-model="$store.preferences.theme" />  <!-- two-way works too -->
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
`#[server]` macro (see `rfc-002`); on the server side the same item
is the real body.

```rust
#[pocopine::server]
pub async fn get_post(id: u32) -> ServerResult<Post> {
    // Runs on the server; on wasm32 this body is replaced with a
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
    pub fn init(&mut self) {
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

### Why not per-field signals?

`signal::<T>()` / `RwSignal<T>` are also in the runtime — close to
React hooks' `useState` or Solid's signals. They are **not** the
canonical component-state shape. Use them for scalar utilities that
live outside any component scope (shared counters, test fixtures,
bits of state that don't earn a whole component). Inside a component:

* `|s, result|` in `dispatch!` handles N fields in one closure; N
  `set_x.set(...)` calls get noisy past two.
* Struct fields auto-serialize to templates (`pp-text="post_id"`);
  signals need a wrapper.
* Rust types on the struct are what users see — signals add a layer.

Struct + `dispatch!` is the one canonical async pattern. If you find
yourself reaching for per-field signals inside a `#[component]`,
promote the data to its own component or use a store instead.

**Do:** model the request's tri-state as `data | loading | error`
sibling fields. That's the opinionated shape; no `Result` enum
exposed to templates.
**Do:** put fetching in `init` for "load once" and in a named handler
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
   `#[watch(field)]` for derivations that need `self`. The framework
   keeps the derived value up to date for you. Templates bind by
   name (`pp-text="progress_label"`). See
   [`docs/poco/04-expressions.md`](../poco/04-expressions.md) for
   the full pattern and the canonical examples.
