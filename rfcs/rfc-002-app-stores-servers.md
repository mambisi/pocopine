# RFC 002 — Application framework, stores, server functions

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-18 |
| **Supersedes** | — |
| **Related** | [`rfc-001-components.md`](./rfc-001-components.md), [`docs/guides/components/02-state.md`](../docs/guides/components/02-state.md) |

## 1. Summary

Three features ship together because they are only useful together:

* **`App`** — a fluent builder that replaces ad-hoc
  `Counter::register(); pocopine::run();` boilerplate. Owns component
  registration, store registration, and before/after-mount hooks.
* **`#[store]`** — a singleton `#[component]`. One instance per type,
  accessible from templates via the `$store` magic path and from Rust
  via `pocopine::store::<T>()`.
* **`#[server]`** — a proc-macro that compiles to two cfg-gated
  definitions: a client stub on wasm32 that POSTs JSON to
  `/_pocopine/<name>` and a server-side handler body plus an
  axum-route helper. A new `pocopine-server` crate re-exports axum /
  tokio / tower-http for the host build.

## 2. Motivation

Before this RFC, building a real app required:

* remembering to call `T::register()` for every component manually;
* reaching into `thread_local!` and `Rc<RefCell>` to share state
  across components, ignoring the reactivity engine;
* hand-writing fetch + serde + HTTP routes twice, once on each side.

All three create friction at the first user scale-up. `App` is a
five-line sugar; `#[store]` is a ten-minute feature; `#[server]` is
the reason the whole framework exists. Ship them together or users
learn three disjoint APIs over three weeks.

## 3. Goals

* One-chain startup: `App::new().register::<…>().store::<…>().run()`.
* Template-level reactivity into and out of stores via `$store.<name>`.
* `#[server] async fn foo(a: A, b: B) -> Result<R, ServerError>`
  available identically on both sides of the wire.
* `pocopine-server` is a drop-in axum helper — not a whole framework.

## 4. Non-goals

* `pp-for` iteration, named slots, deep reactivity (still RFC-001 §8).
* SSR (initial HTML rendered on the server).
* Server-function authentication / session middleware.
* Streaming / SSE / websocket return types from `#[server]`.
* `pocopine-cli dev` proxying API routes to an in-process axum app.
* A macro-level `async fn(&mut self)` handler. Async work still goes
  through the `current_scope_id()` + `spawn_local` escape hatch.

## 5. Design

### 5.1 `App` builder

```rust
pub trait Component { const NAME: &'static str; fn register(); }
pub trait Store: ComponentState + 'static {
    const STORE_NAME: &'static str;
    fn __register_store();
    fn __handle() -> StoreHandle<Self>;
}

pub struct App { /* ... */ }
impl App {
    pub fn new() -> Self;
    pub fn register<C: Component>(self) -> Self;
    pub fn store<S: Store>(self) -> Self;
    pub fn before_mount(self, f: impl FnOnce() + 'static) -> Self;
    pub fn after_mount(self, f: impl FnOnce() + 'static) -> Self;
    pub fn run(self);
}
```

* `before_mount` fires synchronously before the walker starts;
  `after_mount` fires on the next microtask so scopes bound during
  the initial walk are visible.
* `Counter::register()` and `pocopine::run()` still work for ad-hoc
  code paths. `App` is sugar, not a replacement.

### 5.2 Stores

A store is a singleton `#[component]`:

```rust
#[derive(Default, Serialize, Deserialize)]
#[store]
pub struct Preferences { pub theme: String }

#[handlers]
impl Preferences { /* empty or with actions */ }
```

* The macro emits `impl ComponentState + impl Store + thread_local!`
  holding a typed `Rc<RefCell<Preferences>>` plus a scope wrapper in
  the store registry (keyed by name).
* Templates: `$store.preferences.theme` resolves through a dotted-path
  walker (`resolve_path`) that `Reflect::get`s one segment at a time.
  Each intermediate proxy's `get` trap calls `track`, so dep tracking
  traverses the whole path.
* Rust: `pocopine::store::<Preferences>().update(|p| p.theme = ...)`
  mutates and triggers the store's scope.

### 5.3 `#[server]`

```rust
#[pocopine::server]
pub async fn get_post(post_id: u32) -> ServerResult<Post> { /* body */ }
```

Two cfg-gated expansions:

* **wasm32** — the signature with a body of
  `pocopine::fetch::call("/_pocopine/get_post", &(post_id,)).await`.
* **non-wasm32** — the user body, plus a helper
  `pub fn __get_post_route(axum::Router) -> axum::Router` that
  registers `POST /_pocopine/get_post` using `axum::Json<(u32,)>` as
  the body extractor.

Protocol:

* Request body: JSON array of positional args
  (`[]` for zero, `[x]` for one, `[x, y]` for two, …).
* Response body: `serde_json::to_string(&result)` where `result` is
  the function's `Result<R, ServerError>`.
* Content-type: `application/json` on the request.

v0 restrictions the macro enforces:

* No `self` args (free functions only).
* No `&T` / `&mut T` args (owned types only; server must clone or
  take its own data).
* Return type must be `Result<_, ServerError>` (not enforced
  structurally, but `pocopine::fetch::call` will return a
  `ServerError::Network(...)` if it can't decode).

### 5.4 Async handlers via scope-id escape hatch

Handlers stay `fn(&mut self)`. For async work:

```rust
#[handlers]
impl BlogPost {
    pub fn init(&mut self) {
        self.loading = true;
        let id = pocopine::current_scope_id().unwrap();
        let post_id = self.post_id;
        wasm_bindgen_futures::spawn_local(async move {
            let result = get_post(post_id).await;
            if let Some(scope) = Scope::find(id) {
                let mut s = scope.state.borrow_mut();
                /* write fields */
            }
            pocopine::trigger_scope(id);
        });
    }
}
```

`current_scope_id()` is set by `Scope::invoke` for the duration of a
handler call. The `spawn_local` future captures the id, then on
completion looks the scope back up from the registry and updates it.

### 5.5 `pocopine-server` crate

Minimal host-only helpers around axum:

```rust
pub use axum;
pub use tokio;
pub use tower_http;

pub fn static_files(dir: impl AsRef<Path>) -> tower_http::services::ServeDir;
pub async fn serve(router: axum::Router, addr: &str) -> std::io::Result<()>;
```

The crate is cfg-gated to `not(target_arch = "wasm32")` (both its
deps and its body), so the workspace builds cleanly for wasm32 even
when `pocopine-server` is a member.

### 5.6 Example layout: `examples/blog/`

```
examples/blog/
  Cargo.toml        # lib (cdylib) + bin "server"; pocopine-server is target-gated
  src/
    shared.rs       # Post + any other wire types
    lib.rs          # BlogPost component + #[server] get_post
    BlogPost.poco    # template
    bin/
      server.rs     # #[tokio::main] axum entry, calls __get_post_route
  index.html        # mounts <blog-post post-id="1"> and <blog-post post-id="999">
```

`index.html` is served as a static file by the server binary (via
`static_files(".")`). `pocopine-cli dev` is not used for this example
— the axum server IS the dev server.

## 6. Runtime responsibilities

New in `pocopine-core`:

* `app.rs` — `App` builder, `Component` trait, lifecycle hooks.
* `store.rs` — `Store` trait, `StoreHandle<T>`, `store::<T>()`,
  `register_store_scope`, `stores_object` (lazy `$store` cache).
* `path.rs` — `resolve_path` / `write_path` for dotted keys.
* `server.rs` — `ServerError`, `ServerResult<T>`.
* `fetch.rs` — `call<A, R>(url, args) -> ServerResult<R>`.
* `scope.rs` — `CURRENT_SCOPE_ID` thread-local + `current_scope_id()`,
  set around `Scope::invoke`.
* Attribute-name kebab→snake mapping in `walker::apply_static_props`
  (`post-id` → `post_id`).

## 7. Compiler responsibilities

New in `pocopine-macros`:

* `#[component]` also emits `impl Component`.
* `#[store]` — singleton variant of `#[component]` (ComponentState +
  Store + thread_local + registration). Requires a sibling `#[handlers]
  impl` just like components (empty is fine).
* `#[server]` — two cfg-gated expansions; rejects `self` / `&T` /
  non-ident arg patterns.

All three read from `::pocopine::__private::*`; the `#[server]` macro
additionally names `::pocopine_server::axum` on the host side, so
consumers of `#[server]` must add `pocopine-server` as a
target-gated dep.

## 8. Implementation plan

Shipped. See the approved plan at
`.claude/plans/polymorphic-splashing-music.md` and the commit history.
15 ordered steps: 6 for App, 4 for stores, 5 for server functions +
docs. Every step built cleanly on the previous before moving on.

## 9. Alternatives considered

* **Declarative macro app surface** (`pocopine::app![Counter,
  TodoItem, store: Cart]`). Rejected — fluent builder is more Rust-idiomatic
  and extends cleanly to conditional registration and hook methods.
* **DIY server protocol** (ship only the client stub, users wire their
  own axum/actix). Rejected as the default — the per-project
  boilerplate negates most of the `#[server]` win. Still possible:
  `pocopine::fetch::call` alone suffices for users who want to skip
  `pocopine-server`.
* **Async handler macro** that rewrites
  `async fn init(&mut self)` at compile time. Deferred — the current
  `current_scope_id() + spawn_local` pattern is ~8 lines and teaches
  the mental model. When we add async handlers, they become sugar
  over this path.
* **`tiny_http`-based server** (reusing the CLI's server). Rejected —
  tiny_http is sync, which forces awkward `block_on` wrappers in
  server-function bodies. axum is async-native and matches what a
  real app will ship.

## 10. Unresolved questions

* **SSR.** The server should be able to render a component's template
  as HTML (with data inlined) for the initial page response, then let
  the client walker hydrate. Large open design — blocked on deciding
  how templates are streamed through axum's response pipeline.
* **`pocopine-cli dev` ↔ server function proxy.** The current CLI
  serves static files; to proxy `/_pocopine/*` to a user's in-process
  server, we'd need to link the server bin into the CLI or spawn it
  as a subprocess. TBD.
* **Request context.** Headers, cookies, auth — server functions
  currently see only their typed args. A future extension will let
  functions declare an `ExtractorArg<T>` to get request-scoped data.
* **Streaming returns.** `async fn foo() -> impl Stream<Item = T>`
  makes no sense over the current JSON-one-shot protocol. Streaming
  probably implies a separate `#[server(stream)]` marker + SSE or
  websocket transport. Out of scope.

## 11. Migration / impact

* `Counter::register(); pocopine::run();` continues to work. Users are
  encouraged but not forced to migrate to `App::new().…run()`. Existing
  examples were migrated in-RFC.
* No wire-format change — this RFC establishes the initial one.
* The kebab→snake attribute coercion is new; no existing apps have
  attributes that contain hyphens meant to be literal field names,
  so the risk of silently breaking a field-named-with-hyphen is
  effectively zero.
