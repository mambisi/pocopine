# RFC 078 - Client route guards, loaders, and fetch middleware

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-07 |
| **Related** | [`rfc-003-router.md`](./rfc-003-router.md), [`rfc-076-app-plugin-lifecycle.md`](./rfc-076-app-plugin-lifecycle.md), [`rfc-077-server-plugin-lifecycle.md`](./rfc-077-server-plugin-lifecycle.md), [`rfc-074-auth-credentials-and-provider-trait.md`](./rfc-074-auth-credentials-and-provider-trait.md) |
| **Supersedes** | - |

## 1. Summary

Add three generic router/fetch primitives the client side is missing:

1. **Route guards** — synchronous predicates that run before a route paints
   and can `Allow`, `Redirect`, or `Block` navigation.
2. **Route loaders** — async functions that run before component mount,
   produce typed data the component reads, and can fail in
   router-recognized ways (`Unauthorized`, `Forbidden`, `NotFound`,
   `Server`).
3. **Fetch middleware chain** — a tower-style chain of wrappers around
   `pocopine_core::fetch::call` so plugins (auth, telemetry, retry) can
   intercept outgoing `#[server]` calls without component code knowing.

These are deliberately not auth features. They're generic router and
fetch enhancements designed *with* auth as the marquee consumer so the
shapes line up cleanly with what `pocopine-auth-client` will need: a
`Predicate` that works in both `App::route(...).guard(...)` and
`#[server(guard = ...)]`, a `LoaderError::Unauthorized` the router
recognizes as a redirect-to-login signal, and a fetch middleware seam
the auth plugin can hook to handle 401/refresh transparently.

The proposed API surface is:

```rust
// Per-route configuration on the App builder:
App::new()
    .route::<Dashboard>("/dashboard")
        .guard(require_auth())
        .loader(|ctx| async move { ... })
    .route::<AdminPanel>("/admin")
        .guard(require_role("admin"))
    .route::<Login>("/login")
    .login_route("/login")            // configures Unauthorized fallback
    .plugin(auth_plugin())
    .run();

// Loader data extracted in component setup:
#[handlers]
impl Dashboard {
    pub fn on_setup(&mut self, data: Loader<DashboardData>) {
        self.user = data.user.clone();
        self.stats = data.stats.clone();
    }
}

// Fetch middleware (typically installed by a plugin):
fetch::install_middleware(|next| async move {
    let response = next.call().await;
    match &response {
        Err(ServerError::Unauthorized(_)) => {
            // refresh token, then replay
        }
        _ => {}
    }
    response
});
```

## 2. Motivation

RFC-003's open questions (§10) listed route loaders, route-level guards,
and nested layouts as deferred. RFC-076 and RFC-077 closed the plugin
lifecycle gap — but auth (the obvious first plugin consumer) cannot
deliver a smooth experience without these three primitives:

- **No guards** ⇒ every gated component must check auth in `on_setup`,
  call `navigate("/login")` manually, and handle the brief flash of
  the gated component's template before redirect.
- **No loaders** ⇒ component setup spawns server calls; the component
  paints its empty state, then re-paints when the data arrives.
  Suspense-y but inconsistent across routes.
- **No fetch middleware** ⇒ a 401 from a `#[server]` call surfaces as
  `ServerError::Unauthorized` to the calling component, which has to
  decide locally whether to navigate-to-login or retry. The auth
  plugin can't make this transparent.

The same primitives are also useful for non-auth concerns — feature
flags, A/B paint, analytics impression tracking on route entry,
prefetch on hover. Designing them generically keeps the auth integration
honest about what's "auth concern" vs "router concern."

## 3. Goals

- A `Predicate` trait (or `RouteGuard` if separation is needed) usable
  by both client-side route guards and server-side `#[server(guard =
  ...)]` policies — same value, two install points.
- A `Loader` extractor (`LifecycleContext`-style) that reads
  loader-produced data in the component's lifecycle hooks.
- `LoaderError::Unauthorized` recognized as a redirect signal in the
  router; configurable login route via `App::login_route("/login")`
  with a sensible default.
- A `pocopine_core::fetch::install_middleware` chain that wraps every
  outgoing `#[server]` call without component code changing.
- Loaders run **before** the component mounts (Remix-style), so the
  component sees data on first paint or doesn't paint at all on a
  Redirect/Block outcome.
- Cancellation: navigating away from an in-flight loader aborts it.
- Compose with RFC-076's plugin lifecycle — the auth plugin
  registers guards, loaders, and fetch middleware through `AppPlugin`
  install paths, not new top-level APIs.

## 4. Non-goals

- **Auth itself.** This RFC ships generic primitives. `pocopine-auth-client`
  is a follow-up plugin that consumes them.
- **Nested layouts** (sub-outlets, layout components). Orthogonal —
  warrants its own RFC against RFC-003.
- **Server-side rendering of loader data.** SSR is gated on RFC-059 and
  template rendering primitives that don't yet exist.
- **New component lifecycle methods.** Components keep their existing
  `on_setup`/`on_mount`/`on_ready`/`on_unmount` surface; loader data
  flows through extractor types like `Plugin<T>`.
- **Per-route code-splitting.** RFC-065 owns route bundling; loaders
  are about data, not code.
- **Loader cache / SWR / revalidation policies.** Plugins can layer
  these on top using fetch middleware + reactive state; the router
  doesn't ship a built-in cache.

## 5. Design

### 5.1 The `Predicate` trait

Lives in `pocopine-auth` (the shared crate) so it can be used on both
sides:

```rust
pub trait Predicate: Send + Sync + 'static {
    fn check(&self, principal: &Principal) -> Decision;
}

#[derive(Clone, Debug)]
pub enum Decision {
    Allow,
    Deny(&'static str),  // reason: "unauthorized", "forbidden", etc.
}

pub fn require_auth() -> impl Predicate;
pub fn require_role(role: &str) -> impl Predicate;
pub fn require_permission(permission: &str) -> impl Predicate;
pub fn any_of<P: Predicate, Q: Predicate>(p: P, q: Q) -> impl Predicate;
pub fn all_of<P: Predicate, Q: Predicate>(p: P, q: Q) -> impl Predicate;
```

Server-side: `#[server(guard = require_role("admin"))]` already accepts
a function returning `Result<(), ServerError>`. A small adapter
implements `From<Decision> for Result<(), ServerError>` so the same
predicate value works as a guard.

Client-side: `App::route::<C>(path).guard(predicate)` runs the predicate
against the client-side `Principal` mirror (populated by
`pocopine-auth-client`'s `AuthSession`).

The `Predicate` trait is intentionally sync. Most client guards run
against in-memory state; async guard work belongs in a loader, where
the user already has an async context and a richer error surface.

### 5.2 Route guards on the client

```rust
pub struct RouteBuilder<C: Component> { ... }

impl App {
    pub fn route<C: Component>(self, pattern: &'static str) -> RouteBuilder<C> {
        ...
    }
}

impl<C: Component> RouteBuilder<C> {
    pub fn guard(self, predicate: impl Predicate) -> Self;
    pub fn loader<F, T>(self, loader: F) -> Self
    where
        F: for<'a> Fn(LoaderContext<'a>) -> BoxFuture<'a, Result<T, LoaderError>> + 'static,
        T: 'static;
}
```

When the router resolves a navigation:

1. Match the path → component + params (existing behavior).
2. **Run guards in registration order.** First non-`Allow` outcome
   wins:
   - `Allow` ⇒ continue.
   - `Redirect(path)` ⇒ navigate to `path`, no mount, no loader runs.
   - `Block(reason)` ⇒ paint a configurable "blocked" surface (default:
     a small inline error) and emit a `RouteNavigationFailed` event with
     `reason: "guard_blocked"`.
3. **Run the loader** (if any). Returns `Result<T, LoaderError>`:
   - `Ok(data)` ⇒ stash in a per-route `LoaderSlot<C>`, mount the
     component. `Loader<T>` extractor in the component's setup reads
     the slot.
   - `Err(LoaderError::Unauthorized)` ⇒ navigate to the configured
     login route; emit `RouteNavigationFailed { reason:
     "loader_unauthorized" }`.
   - `Err(LoaderError::Forbidden | NotFound | Server(_))` ⇒ paint a
     route-level error surface (configurable; default is a small inline
     error and a corresponding `RouteNavigationFailed` event).
4. Mount the component.

Guards and loaders both receive a small `RouteContext` carrying the
matched path, params, and the reactive `Principal` proxy.

### 5.3 The `LoaderError` enum

```rust
#[derive(Debug)]
pub enum LoaderError {
    Unauthorized,        // → router redirects to login_route
    Forbidden(String),   // → router paints error surface
    NotFound(String),    // → router paints 404 surface (or wildcard route)
    Server(ServerError), // → router paints generic error surface
}

impl From<ServerError> for LoaderError {
    fn from(err: ServerError) -> Self {
        match err {
            ServerError::Unauthorized(_) => LoaderError::Unauthorized,
            ServerError::Forbidden(reason) => LoaderError::Forbidden(reason),
            ServerError::App(reason) | ServerError::BadRequest(reason) => {
                LoaderError::NotFound(reason)
            }
            other => LoaderError::Server(other),
        }
    }
}
```

The `From<ServerError>` impl makes loaders ergonomic: a loader body is
typically `let user = api::current_user().await?;` with `?` surfacing
the right router signal automatically. **Crucially, this is the only
auth-aware piece in the router** — `LoaderError::Unauthorized` is
generic enough that the router doesn't have to know what auth scheme
produced it; the auth plugin's role is to install a fetch middleware
that translates 401 responses into `ServerError::Unauthorized`, and
loaders propagate it upward via `?`.

### 5.4 The `Loader<T>` extractor

```rust
pub struct Loader<T: 'static> {
    data: Rc<T>,
}

impl<'a, T: 'static> From<LifecycleContext<'a>> for Loader<T> { ... }
```

In the component:

```rust
#[handlers]
impl Dashboard {
    pub fn on_setup(&mut self, data: Loader<DashboardData>) {
        self.user = data.user.clone();
        self.stats = data.stats.clone();
    }
}
```

Mirror of `Plugin<T>` with the same `Rc<T>` shape. The data is
populated by the router into a `LoaderSlot<C>` keyed by component
type; the `LifecycleContext::From<Loader<T>>` impl reads the slot.
Slot lifetime: cleared when the route's component unmounts.

`Option<Loader<T>>` is supported for components that want to be
mountable both via routes (with loader data) and via `mount_subtree`
(without).

### 5.5 The fetch middleware chain

```rust
// pocopine_core::fetch
pub trait FetchMiddleware: Send + 'static {
    fn call<'a>(
        &'a self,
        request: FetchRequest,
        next: FetchNext<'a>,
    ) -> BoxFuture<'a, Result<FetchResponse, ServerError>>;
}

pub fn install_middleware<M: FetchMiddleware>(middleware: M);
```

The macro-generated `#[server]` client stub already calls
`fetch::call`. The new middleware chain wraps `fetch::call` so each
installed middleware sees the outgoing request and the response;
middlewares fire in registration order on the way out, reverse order
on the way in (tower convention).

The auth plugin installs one middleware that:
1. Lets the request go through.
2. On `Err(ServerError::Unauthorized)`: if a refresh callback is
   configured, exchange refresh→access, call `client::set_token(new)`,
   replay the request, return the replayed response.
3. If no refresh or refresh fails: clear `AuthSession`, navigate to
   login, return the original `Unauthorized`.

The component author never sees the 401 in the typical case; auth
makes it transparent.

### 5.6 App-level configuration

```rust
impl App {
    /// Set the route the router redirects to when a loader returns
    /// `LoaderError::Unauthorized` (or a guard returns
    /// `Redirect`-without-target). Default: `"/login"`.
    pub fn login_route(self, path: &'static str) -> Self;

    /// Configure the surface painted when a loader returns
    /// `LoaderError::NotFound` and no wildcard route is registered.
    /// Default: small inline 404.
    pub fn not_found_component<C: Component>(self) -> Self;

    /// Configure the surface painted on `LoaderError::Forbidden` /
    /// `Server`. Default: small inline error.
    pub fn route_error_component<C: Component>(self) -> Self;
}
```

Defaults are deliberately minimal so apps work out of the box; real
apps replace them with their own components.

### 5.7 Macro syntax

The `app!{}` macro accepts an extended route entry shape:

```rust
pocopine::app! {
    components: [Dashboard, AdminPanel, Login],
    plugins: [auth_plugin(provider())],
    routes: [
        ("/", Home),
        ("/dashboard", Dashboard, guard = require_auth()),
        ("/admin",     AdminPanel, guard = require_role("admin")),
        ("/login",     Login),
    ],
};
```

Loaders are not expressible inline (they're closures with capture);
the macro emits route registration and the user attaches loaders via
the fluent `App::route(...).loader(...)` builder when needed, or via a
helper plugin.

### 5.8 Cancellation

Each `mount_current` invocation gets a `RouteToken` (monotonic id).
Loaders capture the token and check it before storing data; if the
token doesn't match the router's current token (because navigation
happened during the loader), the result is dropped.

Existing `spawn_latest`-style infrastructure can be reused for the
loader's spawn target.

### 5.9 Loader integration with `pocopine-auth-client`

The auth plugin doesn't need the router to know about auth. It just
needs:

- `Predicate` shared (§5.1) — auth supplies the predicate values.
- `LoaderError::Unauthorized` (§5.3) — auth's fetch middleware
  produces `ServerError::Unauthorized`, loaders propagate via `?`,
  router redirects.
- `fetch::install_middleware` (§5.5) — auth plugin installs the 401
  retry/redirect middleware.
- Reactive `Principal` proxy in `RouteContext` — auth plugin populates
  it; predicates read it. Identity is the only auth-specific concept;
  it lives in `pocopine-auth` (shared crate) and ships
  unchanged.

That's the full integration surface. Everything else is generic
router and fetch infrastructure.

## 6. Phased Plan

### Phase 1 — `Predicate` trait + shared types

- Add `Predicate` and `Decision` to `pocopine-auth`.
- Ship `require_auth`, `require_role`, `require_permission`, `any_of`,
  `all_of` as standard predicates.
- Add `From<Decision> for Result<(), ServerError>` adapter so
  `#[server(guard = …)]` accepts predicates.
- No router changes yet.

### Phase 2 — Client route guards

- `App::route::<C>(...)` returns a `RouteBuilder<C>`.
- `RouteBuilder::guard(predicate)` records the guard.
- Router runs guards in registration order; `Redirect`/`Block`
  outcomes drive navigation/paint.
- Add `RouteNavigationFailed { reason: "guard_blocked" }` /
  `"guard_redirected"` events through the existing RFC-076 plugin
  surface.
- `app!{}` macro accepts `guard = expr` per-route entry.

### Phase 3 — Client route loaders

- `RouteBuilder::loader(...)` records an async loader.
- Router runs loaders after guards, before mount.
- `LoaderError` enum + `From<ServerError>` impl.
- `Loader<T>` extractor wired into `LifecycleContext`.
- Cancellation via `RouteToken`.
- `App::login_route` / `not_found_component` /
  `route_error_component` builders.

### Phase 4 — Fetch middleware chain

- `pocopine_core::fetch::install_middleware`.
- Macro-generated `#[server]` stub passes through the chain.
- `RouteNavigationFailed { reason: "loader_unauthorized" }` event when
  the router catches `LoaderError::Unauthorized`.

### Phase 5 — Documentation + integration tests

- `docs/route-guards-and-loaders.md` walking through the four phases.
- wasm tests covering guard outcomes, loader success/failure paths,
  fetch middleware ordering, and the `Unauthorized` redirect flow.
- Update RFC-074 / RFC-076 to point at this RFC for the predicate trait.

## 7. Privacy and reliability

- Guards are sync and side-effect-free; the router runs them on the
  navigation thread without yielding.
- Loaders run with the same `tracing` / observability surface that
  components have; `RouteNavigationStarted` already fires before the
  loader, `RouteNavigationCompleted` fires after the component mounts.
- Fetch middleware sees the outgoing request body but should not log
  it — same privacy invariant as RFC-077 server events.
- Loader errors do not leak server-side error messages into the
  redirect URL or the painted error surface unless the app
  explicitly opts in via a custom `route_error_component`.

## 8. Open questions

1. **Guard async-ness.** The proposal makes guards sync. Is there a
   real-world auth scenario where a guard needs to await? (Token
   refresh on entry?) If yes, do we add a separate `AsyncGuard` or
   make all guards async?

2. **Predicate location.** Does the `Predicate` trait live in
   `pocopine-auth` (shared) or in a new `pocopine-router` shared
   crate? `pocopine-auth` owns `Principal`, so it's the natural home
   today, but the trait itself isn't auth-specific.

3. **`LoaderError::Unauthorized` semantics.** Should it be a generic
   router primitive (as drafted) or an auth-specific enum variant
   that the router treats opaquely via a configured handler trait?
   The drafted approach is simpler; the alternative is more decoupled
   but adds an auth-shaped extension point.

4. **Loader data lifetime.** Loader data is held in `Rc<T>` per route
   instance. Does this need to outlive the component (e.g. for a
   "back to dashboard" cache)? Or should it be cleared on every
   navigation?

5. **Loader composition.** Should `loader(...)` be allowed multiple
   times per route (parallel loaders, merged into a tuple), or is
   one loader per route enough? Multiple loaders compose ergonomically
   but complicate the `Loader<T>` extractor.

6. **Fetch middleware error surface.** A middleware that returns
   `Err` short-circuits the chain. Is that the right shape, or should
   middleware always return `Result<FetchResponse, ServerError>` and
   let the framework decide whether to pass an error response through
   to the caller?

7. **Macro guard syntax.** `("/admin", AdminPanel, guard =
   require_role("admin"))` works but feels syntactically heavy. Is
   there a cleaner shape (`guard: require_role("admin")` like a struct
   literal)? The current `app!{}` parser accepts colon-separated keys
   for `components:` / `plugins:` / `routes:`.

8. **Default error components.** The defaults paint inline errors. Is
   that the right floor, or should the framework refuse to mount
   without explicit error components configured (RFC-061-style)?

9. **Login redirect with intent.** Should the redirect to login carry
   the original requested path so the login flow can return there?
   Common UX, but adds query-string handling and a "post-login
   redirect" contract the framework would have to honor.

10. **Server-loader symmetry.** `#[server]` already supports guards;
    should `#[server]` also gain a loader-style data-fetching
    primitive, or is that what `#[server]` already is and we shouldn't
    overload it?
