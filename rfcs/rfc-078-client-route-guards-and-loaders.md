# RFC 078 - Client route guards, loaders, and fetch middleware

| Field | Value |
|---|---|
| **Status** | Draft (open questions resolved 2026-05-07; body updated) |
| **Author** | pocopine team |
| **Created** | 2026-05-07 |
| **Related** | [`rfc-003-router.md`](./rfc-003-router.md), [`rfc-076-app-plugin-lifecycle.md`](./rfc-076-app-plugin-lifecycle.md), [`rfc-077-server-plugin-lifecycle.md`](./rfc-077-server-plugin-lifecycle.md), [`rfc-074-auth-credentials-and-provider-trait.md`](./rfc-074-auth-credentials-and-provider-trait.md) |
| **Supersedes** | - |

## 1. Summary

Add three generic router/fetch primitives the client side is missing:

1. **Route guards** — synchronous functions that run before a route
   paints and return one of `Allow`, `Redirect(path)`, or
   `Block(reason)`. The trait lives in `pocopine-core` (the router
   crate) and knows nothing about auth.
2. **Route loaders** — async functions that run before component
   mount, produce typed data the component reads, and can fail in
   router-recognized ways (`Unauthorized`, `Forbidden`, `NotFound`,
   `Server`). `Unauthorized` is a generic routing state — the router
   does not need to know what authentication scheme produced it.
3. **Fetch middleware chain** — a tower-style chain of wrappers
   around `pocopine_core::fetch::call` so plugins (auth, telemetry,
   retry) can intercept outgoing `#[server]` calls. Each middleware
   returns `Result<FetchResponse, ServerError>`; `Err` short-circuits.

These are deliberately not auth features. Auth is the marquee
consumer, but the router crate ships none of the auth concepts.
`pocopine-auth-client` (a follow-up plugin) provides a blanket
`impl<P: Predicate> RouteGuard for P` adapter so a shared `Predicate`
value (defined in `pocopine-auth`) becomes a route guard that
redirects to the configured login route on `Deny`. The same predicate
value works in `#[server(guard = …)]` on the server side via the
existing `Result<(), ServerError>` adapter — one predicate, two
install points.

When the router redirects to the login route (either from a guard's
`Redirect` outcome or from `LoaderError::Unauthorized`) it appends
the original path as a query parameter (default
`?redirect=/originally-requested`) so the auth plugin can navigate
back after sign-in.

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

- A generic `RouteGuard` trait in `pocopine-core` returning
  `RouteGuardDecision::{Allow, Redirect(path), Block(reason)}`.
  No auth-shaped types in the router.
- A separate `Predicate` trait in `pocopine-auth` returning
  `Decision::{Allow, Deny(reason)}`. The same value installs as a
  client route guard (via a blanket adapter shipped by
  `pocopine-auth-client`) **and** as a server `#[server(guard = …)]`
  policy (via the existing `Result<(), ServerError>` adapter).
- A `Loader<T>` extractor (`LifecycleContext`-style) that reads
  loader-produced data in the component's lifecycle hooks. **One
  loader per route** in the first version — multiple async fetches
  compose inside a single loader returning a struct.
- `LoaderError::Unauthorized` recognized as a redirect signal in the
  router; configurable login route via `App::login_route("/login")`
  with a sensible default.
- **Login redirect with intent** — when the router redirects to
  `login_route`, the original path is appended as a query parameter
  (default name `redirect`, configurable via
  `App::login_redirect_param`).
- A `pocopine_core::fetch::install_middleware` chain that wraps every
  outgoing `#[server]` call without component code changing. Middleware
  returns `Result<FetchResponse, ServerError>`; `Err` short-circuits.
- Loaders run **before** the component mounts (Remix-style), so the
  component sees data on first paint or doesn't paint at all on a
  Redirect/Block outcome.
- Loader data has **per-mount lifetime** — cleared when the route's
  component unmounts, no built-in cache. Caching/SWR ships separately
  as a plugin or explicit route option.
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

### 5.1 `RouteGuard` (generic router primitive)

Lives in `pocopine-core` (the router crate). Knows nothing about
auth, principals, or any specific deny semantics:

```rust
// pocopine-core::router::guard
pub trait RouteGuard: Send + Sync + 'static {
    fn decide(&self, ctx: &RouteContext) -> RouteGuardDecision;
}

#[derive(Clone, Debug)]
pub enum RouteGuardDecision {
    /// Continue navigation — run the loader (if any) and mount.
    Allow,
    /// Cancel this navigation and navigate to `path` instead.
    /// Loader does not run; mount does not happen.
    Redirect(String),
    /// Cancel navigation and paint a "blocked" surface.
    /// `reason` is a short stable identifier surfaced in
    /// `RouteNavigationFailed` events.
    Block(&'static str),
}

pub struct RouteContext<'a> {
    pub path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub matched_pattern: Option<&'static str>,
    /// The configured login route, with the original path attached
    /// as the configured intent query parameter. Helpers like
    /// `pocopine-auth-client`'s blanket `RouteGuard` impl use this
    /// to build a `Redirect` outcome.
    pub fn login_route_with_intent(&self) -> String;
}
```

`RouteGuard` is intentionally **sync**. Async guard work belongs in
a loader — loaders already have cancellation, structured error
returns (`LoaderError`), and an async context. A guard's job is to
make a fast in-memory decision (predicate against an in-memory
`AuthSession`, feature-flag check, A/B paint, etc.).

The router does not import any auth types. It calls `decide` and
acts on the outcome.

### 5.1.1 `Predicate` (auth's domain)

Lives in `pocopine-auth` (the shared crate). Returns a smaller
deny-vs-allow surface that's natural for permission checks:

```rust
// pocopine-auth
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

A `Predicate` does not know about routing. It produces a `Decision`
that callers translate into whatever they need: a `Result<(),
ServerError>` for `#[server(guard = …)]`, or a `RouteGuardDecision`
for client routes.

### 5.1.2 Adapters

Two adapter impls bridge the predicate into the two install points:

```rust
// pocopine-auth — server-side adapter (existing pattern, formalized)
impl From<Decision> for Result<(), ServerError> {
    fn from(d: Decision) -> Self {
        match d {
            Decision::Allow => Ok(()),
            Decision::Deny("forbidden") => Err(ServerError::Forbidden(...)),
            Decision::Deny(_) => Err(ServerError::Unauthorized(...)),
        }
    }
}

// pocopine-auth-client — client-side adapter, blanket impl
impl<P: Predicate> RouteGuard for P {
    fn decide(&self, ctx: &RouteContext) -> RouteGuardDecision {
        let principal = active_plugin::<AuthSession>()
            .map(|s| s.principal())
            .unwrap_or_default();
        match Predicate::check(self, &principal) {
            Decision::Allow => RouteGuardDecision::Allow,
            Decision::Deny(_) => {
                RouteGuardDecision::Redirect(ctx.login_route_with_intent())
            }
        }
    }
}
```

The blanket impl is the only auth-shaped piece in the route-guard
chain, and it lives in `pocopine-auth-client` — a crate the router
does not depend on. Apps that don't install the auth plugin can still
use `RouteGuard` directly for non-auth guards (feature flags, A/B
paint, etc.); apps that do install the auth plugin get the
predicate-as-guard sugar for free.

For server symmetry, `#[server(guard = require_role("admin"))]`
keeps its existing function-returning-`Result<(), ServerError>`
contract; the macro generates the call into the predicate via the
`From` adapter above.

### 5.2 Route guards on the client

```rust
pub struct RouteBuilder<C: Component> { ... }

impl App {
    pub fn route<C: Component>(self, pattern: &'static str) -> RouteBuilder<C> {
        ...
    }
}

impl<C: Component> RouteBuilder<C> {
    /// Install a route guard. Accepts any `RouteGuard` value — typed
    /// route guards from the router crate, the auth plugin's
    /// `Predicate`-as-`RouteGuard` blanket impl, custom feature-flag
    /// guards, etc.
    pub fn guard(self, guard: impl RouteGuard) -> Self;

    /// Install a single async loader. Multiple registered loaders
    /// are not supported in the first version — compose multiple
    /// fetches inside one loader returning a struct.
    pub fn loader<F, T>(self, loader: F) -> Self
    where
        F: for<'a> Fn(LoaderContext<'a>) -> BoxFuture<'a, Result<T, LoaderError>>
            + Send + Sync + 'static,
        T: 'static;
}
```

When the router resolves a navigation:

1. Match the path → component + params (existing behavior).
2. **Run guards in registration order.** First non-`Allow` outcome
   wins:
   - `Allow` ⇒ continue.
   - `Redirect(path)` ⇒ navigate to `path`, no mount, no loader runs.
     Emit `RouteNavigationFailed { reason: "guard_redirected" }`.
   - `Block(reason)` ⇒ paint the configured "blocked" surface
     (default: a small inline error using the same minimal component
     as `route_error_component`). Emit `RouteNavigationFailed { reason:
     "guard_blocked" }`.
3. **Run the loader** (if any, and at most one). Returns
   `Result<T, LoaderError>`:
   - `Ok(data)` ⇒ stash in a per-mount `LoaderSlot<C>`, mount the
     component. `Loader<T>` extractor in the component's setup reads
     the slot. The slot is dropped when the component unmounts —
     no caching across navigations in v1.
   - `Err(LoaderError::Unauthorized)` ⇒ navigate to
     `login_route_with_intent()`; emit `RouteNavigationFailed { reason:
     "loader_unauthorized" }`.
   - `Err(LoaderError::Forbidden | NotFound | Server(_))` ⇒ paint a
     route-level error surface (configurable; default is a small inline
     error and a corresponding `RouteNavigationFailed` event).
4. Mount the component.

The `RouteContext` passed to guards and the `LoaderContext` passed to
loaders carry only router-level data (path, params, query,
matched_pattern, plus `login_route_with_intent()` helper). Guards
that need identity reach into `pocopine-auth-client`'s `AuthSession`
themselves — the router stays auth-agnostic.

### 5.3 The `LoaderError` enum

```rust
#[derive(Debug)]
pub enum LoaderError {
    Unauthorized,        // → router redirects to login_route_with_intent()
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
the right router signal automatically.

**`LoaderError::Unauthorized` is a generic routing state, not an
auth implementation detail.** The router only knows "this route
cannot be entered without login" — it does not know what
authentication scheme produced the signal. Any source can produce
it: the auth plugin's fetch middleware (translating server 401
responses), a custom loader that checks an in-memory feature flag,
a third-party token validator, etc. The router's contract is the
enum variant; the meaning of "logged in" is the consumer's concern.

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

**Lifetime: per-mount.** The slot is created when the loader resolves
`Ok(data)` for a navigation, and dropped when the route's component
unmounts. Re-navigating to the same route re-runs the loader and
allocates a fresh slot — no implicit cache, no stale-while-revalidate,
no shared state across navigations. Caching/revalidation policies
ship as plugins or explicit route options in a follow-up RFC; v1
keeps the router free of cache semantics so we don't quietly grow an
SWR layer inside the route lifecycle.

**One loader per route.** `RouteBuilder::loader(...)` may be called at
most once per route in v1; calling it twice is a programmer error
(panic at builder time, before mount). Routes that need multiple
parallel fetches compose them inside one loader:

```rust
.loader(|ctx| async move {
    let (user, stats) = futures::try_join!(
        api::current_user(),
        api::dashboard_stats(),
    )?;
    Ok(DashboardData { user, stats })
})
```

This is intentional: multiple registered loaders complicate the
`Loader<T>` extractor type story (which `T`?), force per-loader
ordering and error-merging conventions on the router, and add a
cancellation surface that's hard to model. One loader returning a
struct keeps the contract small and predictable; users who outgrow
it can compose with `try_join!` / `join!` until a future RFC
introduces multi-loader support with deliberate semantics.

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

**Error contract.** Middleware returns
`Result<FetchResponse, ServerError>`. Returning `Err` short-circuits
the chain — outer middlewares observe the error rather than a
response. The framework does not reinterpret a middleware's `Err`
as a response under any circumstances. This shape matches both auth
(refresh/retry on `Err(Unauthorized)` re-issues the inner call and
returns its `Result` directly) and telemetry (observe error class,
re-raise unchanged). Middlewares that want to swallow an error and
synthesize a response do so explicitly, by returning `Ok(response)`
constructed from the error data; the router does not paper over the
distinction.

The auth plugin installs one middleware that:
1. Lets the request go through (`next.call(request).await`).
2. On `Err(ServerError::Unauthorized)`: if a refresh callback is
   configured, exchange refresh→access, call `client::set_token(new)`,
   replay the request via `next.call(request_clone).await`, return
   the replayed result directly (success or fresh `Err`).
3. If no refresh or refresh fails: clear `AuthSession`, navigate to
   login, propagate the original `Err(Unauthorized)` upward. The
   loader catches it via `LoaderError::Unauthorized`, the router
   redirects (already in flight from step 3's `navigate`).

The component author never sees the 401 in the typical case; auth
makes it transparent.

### 5.6 App-level configuration

```rust
impl App {
    /// Set the route the router redirects to when a loader returns
    /// `LoaderError::Unauthorized` or a guard returns
    /// `RouteGuardDecision::Redirect`. Default: `"/login"`.
    pub fn login_route(self, path: &'static str) -> Self;

    /// Configure the query-parameter name used to carry the original
    /// (pre-redirect) path to the login route. Default: `"redirect"`,
    /// producing URLs like `/login?redirect=/dashboard`. The auth
    /// plugin reads this parameter on its login surface to know where
    /// to navigate after a successful sign-in.
    pub fn login_redirect_param(self, name: &'static str) -> Self;

    /// Configure the surface painted when a loader returns
    /// `LoaderError::NotFound` and no wildcard route is registered.
    /// Default: small inline 404.
    pub fn not_found_component<C: Component>(self) -> Self;

    /// Configure the surface painted on `LoaderError::Forbidden` /
    /// `Server`. Default: small inline error.
    pub fn route_error_component<C: Component>(self) -> Self;
}
```

The router builds redirects via `RouteContext::login_route_with_intent`,
which combines `login_route` with the original path under
`login_redirect_param`:

| `login_route` | original path | result |
|---|---|---|
| `/login` (default) | `/dashboard` | `/login?redirect=/dashboard` |
| `/auth/sign-in` | `/admin/users/42` | `/auth/sign-in?redirect=/admin/users/42` |
| `/login` with `login_redirect_param("next")` | `/dashboard` | `/login?next=/dashboard` |

The original path is URL-encoded and includes the query string of the
originally requested URL so loaders that read query parameters resume
correctly after sign-in. Apps that don't want intent preservation
can pass `login_redirect_param("")` to disable the parameter.

Defaults are deliberately minimal so apps work out of the box without
forcing error-component configuration. Real apps replace them with
their own components — the defaults are intentionally plain so it's
obvious in production that the override hasn't happened yet.

### 5.7 Macro syntax

The `app!{}` macro accepts an extended route entry shape. The
preferred form is colon-keyed, mirroring the existing
`components:` / `plugins:` / `routes:` top-level keys:

```rust
pocopine::app! {
    components: [Dashboard, AdminPanel, Login],
    plugins: [auth_plugin(provider())],
    routes: [
        ("/", Home),
        ("/dashboard", Dashboard, guard: require_auth()),
        ("/admin",     AdminPanel, guard: require_role("admin")),
        ("/login",     Login),
    ],
};
```

If the existing `app!{}` parser cannot accept colon-keyed tuple
elements without significant rework, Phase 2 ships with `guard =
expr` syntax (Rust attribute-meta convention) and the colon form is
filed as non-blocking syntax polish for a follow-up. The choice is
purely cosmetic — both forms compile to the same `RouteBuilder::guard`
call.

Loaders are not expressible inline (they're closures with capture
and an async block); the macro emits route registration and the user
attaches loaders via the fluent `App::route(...).loader(...)`
builder when needed, or via a helper plugin that walks the registered
routes and injects loaders by component type.

### 5.8 Cancellation

Each `mount_current` invocation gets a `RouteToken` (monotonic id).
Loaders capture the token and check it before storing data; if the
token doesn't match the router's current token (because navigation
happened during the loader), the result is dropped.

Existing `spawn_latest`-style infrastructure can be reused for the
loader's spawn target.

### 5.9 Loader integration with `pocopine-auth-client`

The auth plugin's footprint on the router is intentionally tiny:

- **`RouteGuard` trait (§5.1)** is owned by `pocopine-core`. The
  router calls `guard.decide(&ctx)` and acts on the outcome.
  Auth-aware route guards arrive via the blanket
  `impl<P: Predicate> RouteGuard for P` in
  `pocopine-auth-client` (§5.1.2). Apps that don't install
  `pocopine-auth-client` can still use `RouteGuard` directly for
  feature flags, A/B paint, etc.
- **`Predicate` trait (§5.1.1)** lives in `pocopine-auth` (shared
  crate). Auth supplies `require_auth`, `require_role`,
  `require_permission`. The same predicate value works server-side
  via the `From<Decision> for Result<(), ServerError>` adapter and
  client-side via the `RouteGuard` blanket impl.
- **`LoaderError::Unauthorized` (§5.3)** is generic. Auth's fetch
  middleware translates server 401 responses into
  `ServerError::Unauthorized`; loaders propagate via `?`; the router
  redirects to `login_route_with_intent()`. The router never imports
  `pocopine-auth`.
- **`fetch::install_middleware` (§5.5)** is generic. The auth plugin
  installs one middleware that handles 401/refresh/replay; other
  plugins (telemetry, retry policy) install their own middlewares
  on the same chain.
- **Reactive `AuthSession`** is owned by `pocopine-auth-client` and
  exposed as a plugin service via RFC-076's `provide_plugin`. The
  blanket `RouteGuard` impl reads it through `active_plugin::<AuthSession>()`.
  The router does not see `AuthSession`.

That's the full integration surface. Everything else is generic
router and fetch infrastructure.

## 6. Phased Plan

### Phase 1 — Generic `RouteGuard` primitive + auth `Predicate`

- Add `RouteGuard` trait, `RouteGuardDecision` enum, and the
  `RouteContext` carrying `path`, `params`, `query`,
  `matched_pattern`, and `login_route_with_intent()` helper to
  `pocopine-core::router`. Router-only; no auth deps.
- Add `Predicate` trait, `Decision` enum, and the standard
  predicates (`require_auth`, `require_role`,
  `require_permission`, `any_of`, `all_of`) to `pocopine-auth`.
  Shared crate; no router deps.
- Add `From<Decision> for Result<(), ServerError>` adapter so
  `#[server(guard = …)]` accepts predicates server-side.
- (Client-side `Predicate`-as-`RouteGuard` blanket impl ships in
  `pocopine-auth-client` later, not in this RFC.)

### Phase 2 — Client route guards

- `App::route::<C>(...)` returns a `RouteBuilder<C>`.
- `RouteBuilder::guard(impl RouteGuard)` records the guard.
- Router runs guards in registration order; `Allow`/`Redirect`/`Block`
  outcomes drive navigation/paint.
- Add `RouteNavigationFailed { reason: "guard_blocked" }` /
  `"guard_redirected"` events through the existing RFC-076 plugin
  surface.
- `app!{}` macro accepts `guard:` (preferred) or `guard =` (fallback)
  per-route entry.

### Phase 3 — Client route loaders

- `RouteBuilder::loader(...)` records a single async loader (panic
  on second registration for the same route).
- Router runs the loader after guards, before mount.
- `LoaderError` enum + `From<ServerError>` impl.
- `Loader<T>` extractor wired into `LifecycleContext`; per-mount
  lifetime, no cache.
- Cancellation via `RouteToken`.
- `App::login_route` / `App::login_redirect_param` /
  `App::not_found_component` / `App::route_error_component`
  builders.

### Phase 4 — Fetch middleware chain

- `pocopine_core::fetch::install_middleware`.
- Macro-generated `#[server]` stub passes through the chain.
- Middleware contract: `Result<FetchResponse, ServerError>`, `Err`
  short-circuits.
- `RouteNavigationFailed { reason: "loader_unauthorized" }` event
  when the router catches `LoaderError::Unauthorized`.

### Phase 5 — Documentation + integration tests

- `docs/route-guards-and-loaders.md` walking through the four phases.
- wasm tests covering guard outcomes, loader success/failure paths,
  fetch middleware ordering, the `Unauthorized` redirect flow, and
  the login-redirect-with-intent round trip.
- Update RFC-074 / RFC-076 to point at this RFC for the
  `Predicate` trait and the `RouteGuard` primitive.

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

## 8. Resolved questions

The original ten open questions were resolved by council review on
2026-05-07. Each decision is recorded here and reflected in the body
of the RFC. The biggest structural change — separating the generic
`RouteGuard` primitive from auth's `Predicate` — is captured in
§5.1 / §5.1.1 / §5.1.2.

1. **Guard async-ness — sync, by design.** Async guard work is
   modeled as a loader. Loaders already have cancellation,
   structured error returns (`LoaderError`), and an async context;
   adding async to guards would duplicate that surface and split the
   "fast in-memory check" path from the "may await" path. Token
   refresh on entry, network state checks, and similar async work
   belong in a loader returning a small data type (or `()` if all
   the guard wants is the side effect). Reflected in §5.1.

2. **Predicate location — `Predicate` in `pocopine-auth`,
   `RouteGuard` in `pocopine-core`.** The generic router primitive
   stays in core so the router crate has zero auth dependency. Auth
   provides predicate values (`require_role`, etc.) and the blanket
   `impl<P: Predicate> RouteGuard for P` adapter ships in
   `pocopine-auth-client` — neither extension makes the router
   auth-aware. Reflected in §5.1, §5.1.1, §5.1.2, §5.9.

3. **`LoaderError::Unauthorized` semantics — generic.** The variant
   is a routing state ("this route cannot be entered without
   login"), not an auth implementation detail. The auth plugin
   produces it via fetch middleware; the router only knows the
   variant. Reflected in §5.3.

4. **Loader data lifetime — per-mount, no cache in v1.** The
   `LoaderSlot<C>` is created on `Ok(data)` and dropped on unmount.
   Re-navigating re-runs the loader. Caching/SWR/revalidation ships
   later as a plugin or explicit route option; v1 keeps the router
   free of cache semantics. Reflected in §5.4.

5. **Loader composition — one loader per route.** A second
   `RouteBuilder::loader(...)` call panics at builder time. Multiple
   parallel fetches compose inside one loader via
   `futures::try_join!` returning a struct. Multiple registered
   loaders are deferred to a future RFC with deliberate
   ordering/error-merge/cancellation semantics. Reflected in §5.4.

6. **Fetch middleware error surface —
   `Result<FetchResponse, ServerError>`; `Err` short-circuits.**
   Matches both auth (refresh/retry on `Err(Unauthorized)`) and
   telemetry (observe error class, re-raise unchanged). The
   framework does not reinterpret `Err` as a response; middlewares
   that want to swallow an error and synthesize a response do so
   explicitly. Reflected in §5.5.

7. **Macro guard syntax —
   `("/admin", AdminPanel, guard: require_role("admin"))` preferred,
   `guard = …` as Phase 2 fallback if the parser lift is heavy.**
   Either form is purely cosmetic; both compile to the same
   `RouteBuilder::guard` call. Reflected in §5.7.

8. **Default error components — minimal defaults ship.** Apps work
   out of the box without forcing error-component configuration. The
   defaults are intentionally plain so it's obvious in production
   that an override hasn't happened yet. Reflected in §5.6.

9. **Login redirect with intent — yes, default
   `?redirect=<path>`.** The router builds the redirect URL via
   `RouteContext::login_route_with_intent()`. The query parameter
   name is configurable via `App::login_redirect_param("next")` or
   similar; passing `""` disables intent preservation. The
   originally-requested path is URL-encoded and includes its own
   query string so loaders that read query parameters resume
   correctly after sign-in. Reflected in §5.6.

10. **Server-loader symmetry — no new server primitive.** `#[server]`
    is already the server-side data function. Route loaders are a
    client/router orchestration around `#[server]` calls, not a new
    server-side concept. Adding a `#[loader]` macro would overload
    `#[server]`'s role and split the Rust-function-on-the-server
    surface for no clear gain. Reflected in §4 (non-goals).
