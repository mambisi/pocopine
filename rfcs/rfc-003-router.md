# RFC 003 — Client-side SPA router

| Field | Value |
|---|---|
| **Status** | Implemented |
| **Author** | pocopine team |
| **Created** | 2026-04-18 |
| **Supersedes** | — |
| **Related** | [`rfc-002-app-stores-servers.md`](./rfc-002-app-stores-servers.md) |

## 1. Summary

`App::route::<C>(pattern)` maps URL patterns to `#[component]` types.
The walker sees a single `<pp-outlet>` somewhere in the DOM; on every
URL change the router paints the matching component into that outlet,
with captured path params handed in as HTML attributes (so the
existing tag-based mount pipeline applies them as props). A
`pp-route` directive makes `<a>`-tag navigation client-side; `$route`
is a reactive magic exposing `path`, `params`, and `query` to
templates.

The scope, per user decisions in the approved plan: **client-side
only** (SSR is a later milestone), **centralized builder** declaration
(matches the `App` / `store` declaration style).

## 2. Motivation

pocopine shipped components, stores, and server functions, but one
app meant one page. Any real app needs URL-to-component mapping
without a full reload. The existing primitives (tag-based mounting,
the `MutationObserver`-driven cleanup) already do almost all of the
work — the router is a thin layer that decides *what* to mount and
*where*.

## 3. Goals

* `App::new().route::<Home>("/").route::<Blog>("/blog/:id").run()`.
* Zero boilerplate around history management — the router installs a
  `popstate` listener and paints the initial URL.
* Path params flow into components as real Rust fields via the
  existing prop pipeline. No new plumbing.
* Templates can read `$route.path` / `$route.params.id` reactively.
* Server-function URLs (`/_pocopine/*`) never collide with page
  routes — they are passed through to the browser's default behavior.

## 4. Non-goals

* Server-rendered initial HTML (SSR).
* Nested layouts (layout components with their own `<pp-outlet>`).
* Route-level loader functions (Remix `loader` / SolidStart).
* Typed query params — `$route.query.foo` is always a string.
* Catchall patterns beyond a bare `"*"` fallback.
* View-transitions API, scroll restoration, anchor fragments.
* A `#[route]` attribute macro. Declarations stay centralized in the
  builder chain to match `App::register` / `App::store`.

## 5. Design

### 5.1 Pattern syntax

| Pattern | Meaning |
|---|---|
| `"/"` | root |
| `"/about"` | literal segment |
| `"/blog/:id"` | literal + `:name` capture; exposes `id` as a path param |
| `"/users/:uid/posts/:pid"` | multiple captures |
| `"*"` | 404 fallback — matches any path, tried after all others |

Matching walks segments left-to-right; literal vs capture are
position-matched. The number of `/`-separated segments must equal the
pattern's (no catchall slurp in v0).

### 5.2 Builder surface

```rust
App::new()
    .register::<AppShell>()           // the shell that contains <pp-outlet>
    .route::<Home>("/")
    .route::<About>("/about")
    .route::<BlogPost>("/blog/:id")
    .route::<NotFound>("*")
    .run();
```

`register::<C>` is still how you wire up non-route components (shells,
widgets). `route::<C>(pattern)` does a `register` under the hood plus
a `router::register_route`. Order is preserved; wildcards are always
tried last regardless of position.

### 5.3 Outlet

`<pp-outlet>` is a reserved sentinel tag. The walker recognises it
and hands its element to the router. It takes no attributes, holds no
children (the router owns its subtree). Exactly one outlet per app in
v0; nested outlets are a later RFC.

The `#[component]` macro rejects struct idents whose kebab-case would
collide with `pp-outlet` via the existing HTML5 collision check
(extended to cover framework-reserved tags).

### 5.4 Path params as props

When the router mounts a page, it creates a `<component-name>` element
inside the outlet and calls `set_attribute(key, value)` for each
captured param. It then delegates to `walker::walk` — the existing
tag-based mount path handles scope creation, template clone, slot
capture, and attribute-to-prop coercion (kebab→snake,
bool/number/JSON, per RFC-002 §5.7).

Path param names must be valid attribute names AND convertible to
Rust snake_case field names — the existing walker rule (`post-id` →
`post_id`) applies uniformly.

### 5.5 `pp-route` directive

Intercepts clicks on `<a>` with an `href`. Calls
`router::navigate(href)` + `preventDefault` when all of the following
hold:

* No modifier key (ctrl, cmd, shift, alt).
* Primary mouse button (button 0).
* `target != "_blank"`.
* `href` is not absolute (no `http://`, `https://`, `//`, `mailto:`,
  `tel:`, `data:`, `ws://`, `wss://`).
* `href` does not start with `/_pocopine/` (server-fn routes are not
  pages).

Otherwise the directive falls through and the browser handles the
click as normal.

### 5.6 `$route` magic

Templates read `$route.path`, `$route.params.<name>`, and
`$route.query.<name>`. Backed by a synthetic `RouteState`
`ComponentState` scope held in a `OnceCell` — the router fires
`trigger_scope(scope.id)` on every navigation so any effect
subscribing via dotted-path read reruns.

`$route` is **read-only**. Writing to `$route.path = "/foo"` from a
template has no effect; use `pocopine::navigate("/foo")` from Rust.

### 5.7 Navigation helpers

```rust
pocopine::navigate("/blog/42");   // pushState + re-mount
pocopine::register_route(...);    // low-level; prefer App::route
```

`navigate` is also what the `pp-route` directive calls internally.

### 5.8 Unmount semantics

When the router paints a new page, it uses
`outlet.replace_children_with_node_1(new_el)`. That triggers the
existing `MutationObserver` with the previous subtree in
`removedNodes`, which flows into `walker::release_subtree`:

* All effects pinned to elements via `track_effect_on` are
  `release`-d.
* Scopes owned by those elements are removed from `SCOPES`.

No manual bookkeeping in the router. The invariant is the same one
that makes the walker's live DOM mutation case correct.

### 5.9 Reserved URL space

pocopine reserves the `/_pocopine/*` URL namespace for server
functions (RFC-002 §5.3). The router:

* Never matches a route inside `/_pocopine/*` (users should not
  declare such patterns; doing so is not prevented but is shadowed
  by server-fn URL reservation).
* `pp-route` explicitly bypasses interception for `/_pocopine/*` hrefs
  so navigation to a server-fn URL is always a normal browser request.

## 6. Runtime responsibilities

New in `pocopine-core`:

* `router.rs` — route table, synthetic `RouteState` scope, `navigate`,
  `init`, `match_route`, `popstate` listener, query parser.
* `directives/route.rs` — `pp-route` click handler.

Changes to existing modules:

* `walker::bind` — early return when `el.local_name() == "pp-outlet"`,
  handing the element to `router::set_outlet`.
* `walker::walk` — made `pub` so the router can walk a freshly
  created custom-element tag.
* `magics::resolve` — `"$route"` branch returns `router::route_proxy`.
* `app.rs` — `App::route::<C>(pattern)` + `App::run` calls
  `router::init()` when any route is registered.
* `lib.rs` — re-exports `navigate`, `register_route`.
* `Cargo.toml` — web-sys features: `History`, `HtmlAnchorElement`,
  `Location`, `MouseEvent`, `PopStateEvent`.

## 7. Testing

Pattern parsing + matching has host-target unit tests in
`router::tests` (`cargo test -p pocopine-core --lib`):

* literal match success / failure
* param capture
* mixed segments
* wildcard matches any path
* query-string parsing with percent-decode

DOM-level behavior is exercised by `examples/spa/`.

## 8. Example

See `examples/spa/`: a four-page app (`/`, `/about`, `/blog/:id`, `*`)
with a nav, the `$route.path` displayed as a footer, and the
`BlogPost` component consuming `id` as a prop.

## 9. Alternatives considered

* **File-system routing** (Next.js: `src/pages/Home.rs`). Rejected —
  requires a build-time directory scan (build.rs or proc-macro IO)
  and introduces hidden convention. The explicit builder lists every
  URL in one place.
* **Per-component `#[route]` attribute**. Rejected — finding "what is
  at `/foo`" would require grepping the repo instead of reading one
  `App::new()` chain.
* **Separate outlet component type** (`#[outlet]`). Rejected — a
  reserved tag name is strictly simpler and doesn't ask users to
  register a type that doesn't hold state.

## 10. Unresolved

* **SSR.** The server should be able to pick the matched page and
  render its template into the initial HTML response. Blocked on the
  same template-rendering question as RFC-002 §10.
* **Nested layouts.** Today a single outlet works. A shell with a
  header and a footer can still swap its main body — but a multi-level
  layout (admin area with its own sub-nav + sub-outlet) needs
  parent/child route semantics.
* **Route data loaders.** Loader-style data fetching before mount
  (Remix / SolidStart). Today: fetch inside `init` with `dispatch!`.
* **Prefetching / pending UI.** A single loading indicator works
  (`$route.path` changes instantly on click). Richer pending states
  need loaders to land first.
* **Trailing-slash normalization.** `/about` vs `/about/` match
  different routes today; we may want to coerce one to the other at
  the `navigate` layer.
