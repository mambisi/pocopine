# RFC 078 - Client route guards, loaders, and fetch middleware

| Field | Value |
|---|---|
| **Status** | Draft (open questions resolved 2026-05-07; extensibility redesign applied) |
| **Author** | pocopine team |
| **Created** | 2026-05-07 |
| **Related** | [`rfc-003-router.md`](./rfc-003-router.md), [`rfc-076-app-plugin-lifecycle.md`](./rfc-076-app-plugin-lifecycle.md), [`rfc-077-server-plugin-lifecycle.md`](./rfc-077-server-plugin-lifecycle.md), [`rfc-074-auth-credentials-and-provider-trait.md`](./rfc-074-auth-credentials-and-provider-trait.md) |
| **Supersedes** | - |

## 1. Summary

Add three generic router/fetch primitives the client side is missing:

1. **Route guards** — synchronous functions that run before a route
   paints and return one of `Allow`, `Redirect(target)`, or
   `Reject(rejection)`. The trait lives in `pocopine-core` (the
   router crate) and knows nothing about auth.
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
returns `RouteRejection::Unauthorized` on `Deny`. The same predicate
value works in `#[server(guard = …)]` on the server side via the
existing `Result<(), ServerError>` adapter — one predicate, two
install points.

The router does **not** grow auth-shaped `App::login_route` or
`App::login_redirect_param` methods. Instead it exposes a generic
route-rejection extension chain. Plugins install handlers for
rejections they understand: the auth plugin maps `Unauthorized` to
its configured login UX (route redirect, modal, external IdP, tenant
chooser, MFA step-up, etc.); other plugins can map `Forbidden` to
"request access", feature flags can block behind experiment surfaces,
and the core fallback paints generic error UI. Redirect intent
validation is therefore an extension contract, not an app-shell
method.

The proposed API surface is:

```rust
// Route behavior lives with the component:
impl RouteComponent for Dashboard {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(require_auth())
            .loader(|ctx| async move { ... })
    }
}

impl RouteComponent for AdminPanel {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(require_role("admin"))
    }
}

impl RouteComponent for Login {}

// App wiring stays a small composition shell:
App::new()
    .route::<Dashboard>("/dashboard")
    .route::<AdminPanel>("/admin")
    .route::<Login>("/login")
    .plugin(
        auth_plugin()
            .login_route("/login")
            .return_to_query_param("redirect")
    )
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

- A `RouteComponent` trait in `pocopine-core` for components that
  declare route-local behavior with `fn config() -> RouteConfig<Self>`.
  `App::route::<C>` calls this trait hook at registration time.
- A generic `RouteGuard` trait in `pocopine-core` returning
  `RouteGuardDecision::{Allow, Redirect(target), Reject(rejection)}`.
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
- `LoaderError::Unauthorized` recognized as a route rejection signal.
  The router delegates it to the generic route-rejection extension
  chain; the auth plugin decides whether that means redirect to a
  login route, show a modal, start an external IdP flow, or block.
- **Extension-owned redirect intent** — plugins that redirect after
  a rejection may preserve the original path, but the core router
  only supplies a validated `ReturnTo` helper. Auth chooses the
  parameter name, destination route, and post-login behavior.
- A `pocopine_core::fetch::install_middleware` chain that wraps every
  outgoing `#[server]` call without component code changing. Middleware
  returns `Result<FetchResponse, ServerError>`; `Err` short-circuits.
- Loaders run **before** the component mounts (Remix-style), so the
  component sees data on first paint or doesn't paint at all on a
  Redirect/Reject outcome.
- Loader data has **per-mount lifetime** — cleared when the route's
  component unmounts, no built-in cache. Caching/SWR ships separately
  as a plugin or explicit route option.
- Cancellation: navigating away from an in-flight loader aborts it.
- Compose with RFC-076's plugin lifecycle — the auth plugin
  registers guards, route-rejection handlers, loaders, and fetch
  middleware through `AppPlugin` install paths, not auth-shaped
  top-level APIs on `App`.
- A non-optional security contract (§5.10) covering: client guards
  are UX-only, extension redirect intent is path-only, fetch middleware
  is privileged code that freezes at boot, replay-after-Unauthorized
  is fail-closed by default, cancellation is actual abort plus a
  session-epoch check, sign-out re-evaluates guards, loader-error
  defaults are generic and event reasons are stable.

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

> **Security note (see §5.10.1):** Route guards are UX, not the
> security boundary. They prevent paint and reduce flicker; they
> cannot protect data or server functions. Every protected
> `#[server]` function MUST carry its own `#[server(guard = …)]`
> policy. The same `Predicate` value works in both sites — the
> symmetry is intentional, but the server-side check is the only
> one an attacker cannot bypass by editing wasm or
> `localStorage`.

Lives in `pocopine-core` (the router crate). Knows nothing about
auth, principals, or any specific deny semantics:

```rust
// pocopine-core::router::guard
pub trait RouteGuard: 'static {
    fn decide(&self, ctx: &RouteContext) -> RouteGuardDecision;
}

#[derive(Clone, Debug)]
pub enum RouteGuardDecision {
    /// Continue navigation — run the loader (if any) and mount.
    Allow,
    /// Cancel this navigation and delegate the outcome to the
    /// route-rejection extension chain. Loader does not run; mount
    /// does not happen.
    Reject(RouteRejection),
    /// Cancel this navigation and navigate to a concrete target
    /// immediately. Use sparingly; auth-style redirects should
    /// normally flow through `Reject(Unauthorized)` so plugins own
    /// the UX.
    Redirect(RouteTarget),
}

#[derive(Clone, Debug)]
pub enum RouteRejection {
    Unauthorized,
    Forbidden(&'static str),
    Blocked(&'static str),
    NotFound,
    Server(&'static str),
    Custom { reason: &'static str },
}

pub struct RouteTarget(String);

impl RouteTarget {
    /// Fallible constructor for app-local redirect targets.
    /// Accepts `/path`, `/path?query`, and `/path#hash`; rejects
    /// empty values, protocol-relative URLs (`//host/path`), external
    /// URLs, and backslash-shaped browser URL ambiguity.
    pub fn new(path: impl Into<String>) -> Result<Self, RouteTargetError>;

    /// Ergonomic constructor for static, trusted app-local targets.
    /// Panics on invalid input.
    pub fn path(path: impl Into<String>) -> Self;
}

pub enum RouteTargetError {
    Empty,
    NotAppLocalPath,
}

pub struct RouteContext<'a> {
    pub path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub matched_pattern: Option<&'static str>,
}
```

`RouteGuard` is intentionally **sync** and client-local. It does not
require `Send + Sync`, because browser-side guards often capture
`Rc`-backed app state or local signals. Async guard work belongs in a
loader — loaders already have cancellation, structured error returns
(`LoaderError`), and an async context. A guard's job is to make a
fast in-memory decision (predicate against an in-memory `AuthSession`,
feature-flag check, A/B paint, etc.).

The router does not import any auth types. It calls `decide` and
acts on the outcome. `RouteRejection` is intentionally generic:
it describes why route control stopped, not what UX should happen
next.

### 5.1.1 Route rejection extensions

The core app shell exposes one generic extension point for route
failures. This is the replacement for auth-shaped `App::login_route`
and `App::login_redirect_param` methods.

```rust
pub trait RouteRejectionHandler: 'static {
    /// Return `Some(action)` when this handler owns the rejection.
    /// Return `None` to let the next handler decide.
    fn handle(
        &self,
        ctx: &RouteRejectionContext,
        rejection: &RouteRejection,
    ) -> Option<RouteRejectionAction>;
}

pub struct RouteRejectionContext<'a> {
    pub path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub matched_pattern: Option<&'static str>,
}

#[derive(Clone, Debug)]
pub enum RouteRejectionAction {
    Redirect(RouteTarget),
    Paint(RouteErrorSurface),
    AbortNavigation,
}

pub struct RouteErrorSurface {
    pub title: &'static str,
    pub message: &'static str,
}

impl RouteErrorSurface {
    pub const fn new(title: &'static str, message: &'static str) -> Self;
}

impl App {
    /// Install a route-rejection handler. Plugins call this from
    /// `AppPlugin::install`; applications may also install their own
    /// final policy directly.
    pub fn route_rejection_handler<H: RouteRejectionHandler>(self, handler: H) -> Self;
}
```

Handlers are client-local for the same reason as guards: they often
capture app state or plugin services backed by `Rc`. They run in plugin
install order. The first handler that returns `Some(action)` owns the
rejection. If no handler accepts it, the core fallback paints generic,
non-leaking UI for `Unauthorized`, `Forbidden`, `NotFound`, and
`Server`, and emits a stable failure reason. This makes route
rejection extensible without making the base `App` permanently carry
auth concepts.

### 5.1.2 `Predicate` (auth's domain)

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

### 5.1.3 Adapters

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
    fn decide(&self, _ctx: &RouteContext) -> RouteGuardDecision {
        let principal = active_plugin::<AuthSession>()
            .map(|s| s.principal())
            .unwrap_or_default();
        match Predicate::check(self, &principal) {
            Decision::Allow => RouteGuardDecision::Allow,
            Decision::Deny(_) => {
                RouteGuardDecision::Reject(RouteRejection::Unauthorized)
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
predicate-as-guard sugar for free. The auth plugin separately
installs a `RouteRejectionHandler` that maps
`RouteRejection::Unauthorized` to the app's configured login UX.

For server symmetry, `#[server(guard = require_role("admin"))]`
keeps its existing function-returning-`Result<(), ServerError>`
contract; the macro generates the call into the predicate via the
`From` adapter above.

### 5.2 Route components on the client

```rust
pub trait RouteComponent: Component {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
    }
}

pub struct RouteConfig<C: Component> { ... }

impl App {
    /// Register a route component. The component's `RouteComponent`
    /// implementation supplies route-local guards/loaders.
    pub fn route<C: RouteComponent>(self, pattern: &'static str) -> Self {
        ...
    }

    /// Escape hatch for one-off route configuration at the call site.
    /// Most components should prefer `impl RouteComponent`.
    pub fn route_with<C: Component>(
        self,
        pattern: &'static str,
        config: RouteConfig<C>,
    ) -> Self {
        ...
    }
}

impl<C: Component> RouteConfig<C> {
    pub fn new() -> Self;

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

Route-local behavior is therefore authored next to the component:

```rust
impl RouteComponent for Dashboard {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(require_auth())
            .loader(load_dashboard)
    }
}
```

Plain route components opt into the trait with an empty impl:

```rust
impl RouteComponent for Login {}
```

This is trait-driven wiring, not runtime discovery. Stable Rust
cannot ask "does this type implement `RouteComponent`?" and branch
inside `App::route`; the bound is explicit. If a component is routed
with `App::route::<C>`, it implements `RouteComponent`. If an app
needs inline policy for a single route, it uses `App::route_with`
with an explicit `RouteConfig<C>`.

When the router resolves a navigation:

1. Match the path → component + params (existing behavior).
2. **Run guards in registration order.** First non-`Allow` outcome
   wins:
   - `Allow` ⇒ continue.
   - `Redirect(target)` ⇒ navigate to `target`, no mount, no loader
     runs. Emit `RouteNavigationFailed { reason: "guard_redirected" }`.
   - `Reject(rejection)` ⇒ delegate to the route-rejection extension
     chain. If no extension handles it, the core fallback paints a
     generic error surface. Emit `RouteNavigationFailed` with the
     stable reason mapped from the rejection (for example
     `"guard_blocked"` or `"guard_unauthorized"`).
3. **Run the loader** (if any, and at most one). Returns
   `Result<T, LoaderError>`:
   - `Ok(data)` ⇒ stash in a per-mount `LoaderSlot<C>`, mount the
     component. `Loader<T>` extractor in the component's setup reads
     the slot. The slot is dropped when the component unmounts —
     no caching across navigations in v1.
   - `Err(LoaderError::Unauthorized)` ⇒ delegate
     `RouteRejection::Unauthorized` to the route-rejection extension
     chain; emit `RouteNavigationFailed { reason:
     "loader_unauthorized" }`.
   - `Err(LoaderError::Forbidden | NotFound | Server(_))` ⇒ paint a
     route-level error surface via the same rejection chain
     (configurable by plugin or app; core fallback is small generic
     copy) and a corresponding `RouteNavigationFailed` event.
4. Mount the component.

The `RouteContext` passed to guards and the `LoaderContext` passed to
loaders carry only router-level data (path, params, query,
matched_pattern, plus a validated `return_to()` helper). Guards that
need identity reach into `pocopine-auth-client`'s `AuthSession`
themselves — the router stays auth-agnostic.

### 5.3 The `LoaderError` enum

```rust
#[derive(Debug)]
pub enum LoaderError {
    Unauthorized,        // → route-rejection extension chain
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
  cannot proceed without an extension decision" — it does not know
  what authentication scheme produced the signal. Any source can produce
it: the auth plugin's fetch middleware (translating server 401
responses), a custom loader that checks an in-memory feature flag,
a third-party token validator, etc. The router's contract is the
enum variant; the meaning of "logged in" is the consumer's concern.

> **Security note (see §5.10.7):** the `String` payloads on
> `Forbidden`, `NotFound`, and `Server` may carry server-internal
> messages (stack traces, query strings, internal paths). The
> framework's default `route_error_component` /
> `not_found_component` MUST render generic copy and never
> interpolate the message string. `RouteNavigationFailed` events
> carry only stable reason identifiers from a closed set;
> messages never enter framework events.

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

**One loader per route.** `RouteConfig::loader(...)` may be called at
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
2. On `Err(ServerError::Unauthorized)`, **only if the request is
   replay-safe** (see §5.10.4): if a refresh callback is
   configured, exchange refresh→access through a single-flight
   gate (so concurrent 401s share one refresh round trip), call
   `client::set_token(new)`, replay the request via
   `next.call(request_clone).await` **at most once**, return the
   replayed result directly (success or fresh `Err`).
3. If the request is **not** replay-safe (the default for any
   `#[server]` function not explicitly marked
   `#[server(idempotent)]`), the middleware does **NOT** retry —
   it propagates `Err(Unauthorized)` upward unchanged. POST is
   non-idempotent and a server function that partially completed
   before returning 401 has already taken effect; silent replay
   would duplicate side effects. The loader catches
   `LoaderError::Unauthorized`, the router delegates
   `RouteRejection::Unauthorized` to the rejection chain, and the
   active extension decides whether the user re-issues the action
   after sign-in or another recovery flow.
4. If refresh fails (or no refresh is configured): clear
   `AuthSession` and propagate the original `Err(Unauthorized)`
   upward. If the call belongs to a loader, the route-rejection chain
   handles it; otherwise the application sees the error directly and
   session-change re-evaluation still unmounts newly invalid routes.

> **Security contracts (see §5.10.3, §5.10.4):**
> * `fetch::install_middleware` freezes at first `App::run`;
>   later calls panic.
> * Middleware is trusted-plugin code on par with `App::plugin`;
>   apps should not grant install rights to untrusted
>   dependencies.
> * Telemetry middleware defaults to redacted metadata (function
>   name, status, duration, error class). Bodies, headers,
>   cookies, query strings are never in the default exporter
>   payload.
> * Auth replay is single-flight, max-one-retry, replay-safe-only.

The component author never sees the 401 for replay-safe requests;
for non-replay-safe requests the auth middleware fails closed
rather than risking a double-mutation, and the user is asked to
re-confirm the action.

### 5.6 Extension-owned route rejection policy

The base `App` exposes only generic route-control extension points.
It does not know what "login" means and does not own return-intent
query parameters.

```rust
impl App {
    /// Add a handler to the route-rejection chain. Plugins use this
    /// to map router-level rejections to their own UX.
    pub fn route_rejection_handler<H: RouteRejectionHandler>(self, handler: H) -> Self;

    /// Optional generic fallback surface for rejections nobody
    /// handles. Defaults are small generic copy and never interpolate
    /// error strings. See §5.10.7.
    pub fn route_error_component<C: Component>(self) -> Self;

    /// Optional generic fallback 404 surface. Defaults are small
    /// generic copy and never interpolate error strings.
    pub fn not_found_component<C: Component>(self) -> Self;
}
```

`pocopine-auth-client` owns auth UX as plugin configuration:

```rust
App::new()
    .plugin(
        auth_plugin()
            .login_route("/login")
            .return_to_query_param("redirect")
            .return_to_validation(ReturnToValidation::PathOnly)
    )
    .run();

pub enum AuthUnauthorizedAction {
    /// `/login?redirect=/dashboard`
    RedirectToLogin,
    /// Paint/open a login component without changing routes.
    ShowLoginModal,
    /// Hand control to an external IdP flow.
    ExternalProvider,
    /// Leave the current route unmounted and let the app decide.
    Abort,
}
```

The same rejection can be handled differently by different apps:

```rust
App::new()
    .plugin(auth_plugin().login_route("/signin"))
    .route_rejection_handler(|ctx, rejection| match rejection {
        RouteRejection::Forbidden(_) => Some(RouteRejectionAction::Paint(
            RouteErrorSurface::new(
                "Access requested",
                "Your account needs access before this route can open.",
            ),
        )),
        _ => None,
    })
    .run();
```

Route-rejection handlers run in plugin install order and use
first-handler-wins semantics. This keeps extension policy
composable: auth can own `Unauthorized`, an access-control plugin
can own `Forbidden`, a feature-flag plugin can own `Blocked`, and
the core fallback still handles anything nobody claims.

Redirect intent is represented by `ReturnTo`, a path-only value the
router builds from `window.location.pathname + window.location.search`
and path-validates before any plugin sees it:

```rust
pub struct ReturnTo { /* opaque, path-only */ }

impl ReturnTo {
    pub fn none() -> Self;
    pub fn as_path_and_query(&self) -> &str;
    pub fn append_to(&self, target: RouteTarget, param: &'static str) -> RouteTarget;
}

pub enum ReturnToValidation {
    PathOnly,
    RegisteredRoutes,
}
```

The auth plugin decides whether to append it, what query parameter
to use, whether to apply stricter registered-route validation, or
whether to ignore it entirely. The validation rules live in §5.10.2
and apply to every extension that consumes `ReturnTo`. The important
boundary: the router owns safe path capture and path-only validation;
plugins own UX and any stricter policy.

### 5.7 `app!{}` route syntax

The `app!{}` macro stays thin: route entries name path + component,
and the component's `RouteComponent::config()` supplies guards and
loaders. The macro should not become another inline policy language.

```rust
pocopine::app! {
    components: [Dashboard, AdminPanel, Login],
    plugins: [auth_plugin(provider())],
    routes: [
        ("/", Home),
        ("/dashboard", Dashboard),
        ("/admin",     AdminPanel),
        ("/login",     Login),
    ],
};
```

For advanced one-off routes outside `app!{}`, application code can
still use `App::route_with::<C>(path, RouteConfig::new().guard(...))`.
That escape hatch is intentionally not part of the macro's first
version; `RouteComponent` keeps the common path local to the
component and keeps `app!{}` declarative.

### 5.8 Cancellation

Cancellation has two layers: a `RouteToken` to identify which
navigation a loader belongs to, and an `AbortSignal` to actually
stop in-flight network requests. Both layers are required —
dropping the result without aborting the request would still let
the request hit the server with the previous identity's
credentials, which is unsafe for the post-sign-out window
(§5.10.5).

```rust
pub struct LoaderContext<'a> {
    pub path: &'a str,
    pub params: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    /// Abort signal for this navigation. Loaders pass it into
    /// `fetch::call(request.with_signal(ctx.abort_signal()))`
    /// so the underlying `window.fetch` is cancelled on
    /// supersession.
    pub fn abort_signal(&self) -> AbortSignal;
}

pub struct FetchRequest {
    // ... existing fields ...
    pub abort_signal: Option<AbortSignal>,
}
```

Cancellation flow:

1. The router mints a fresh `RouteToken` and `AbortController` per
   `mount_current` invocation.
2. The loader runs with `LoaderContext` carrying both. It passes
   the `AbortSignal` into every `fetch::call` it makes.
3. When navigation supersedes the loader (user clicked another
   link, browser back, programmatic `navigate`), the router
   **aborts the controller first**, then bumps `RouteToken`.
   `web_sys::AbortController::abort` triggers the underlying
   `window.fetch` to reject with a `DOMException`; the auth
   middleware sees `Err(ServerError::Network(_))` and propagates;
   the loader returns `Err(LoaderError::Server(_))`; the router
   notices the stale `RouteToken` and drops the result without
   painting an error surface.
4. **Session-epoch check** (§5.10.5): the auth middleware also
   captures the current `AuthSession` epoch when building the
   request. On dispatch and on response, if the captured epoch
   doesn't match the live one (the user signed in/out/refreshed
   while the request was in flight), the middleware aborts before
   dispatch (so stale `Authorization` is never sent) and drops
   the response after dispatch (so a body computed for the wrong
   identity is never returned).

Existing `spawn_latest`-style infrastructure can be reused for the
loader's spawn target; the `AbortController` is the new piece this
RFC requires the framework to ship.

### 5.9 Loader integration with `pocopine-auth-client`

The auth plugin's footprint on the router is intentionally tiny:

- **`RouteGuard` trait (§5.1)** is owned by `pocopine-core`. The
  router calls `guard.decide(&ctx)` and acts on the outcome.
  Auth-aware route guards arrive via the blanket
  `impl<P: Predicate> RouteGuard for P` in
  `pocopine-auth-client` (§5.1.3). Apps that don't install
  `pocopine-auth-client` can still use `RouteGuard` directly for
  feature flags, A/B paint, etc.
- **`Predicate` trait (§5.1.2)** lives in `pocopine-auth` (shared
  crate). Auth supplies `require_auth`, `require_role`,
  `require_permission`. The same predicate value works server-side
  via the `From<Decision> for Result<(), ServerError>` adapter and
  client-side via the `RouteGuard` blanket impl.
- **`LoaderError::Unauthorized` (§5.3)** is generic. Auth's fetch
  middleware translates server 401 responses into
  `ServerError::Unauthorized`; loaders propagate via `?`; the router
  delegates `RouteRejection::Unauthorized` to the route-rejection
  extension chain. The auth plugin handles that rejection according
  to its own config (`login_route`, modal login, external IdP, etc.).
  The router never imports `pocopine-auth`.
- **Route-rejection handler (§5.1.1, §5.6)** is installed by the
  auth plugin. This is where auth-owned UX lives: login route,
  return-intent parameter, redirect validation strictness, modal
  login, tenant chooser, MFA step-up, and post-login return policy.
  None of those become methods on core `App`.
- **`fetch::install_middleware` (§5.5)** is generic. The auth plugin
  installs one middleware that handles 401/refresh/replay; other
  plugins (telemetry, retry policy) install their own middlewares
  on the same chain.
- **Reactive `AuthSession`** is owned by `pocopine-auth-client` and
  exposed as a plugin service via RFC-076's `provide_plugin`. The
  blanket `RouteGuard` impl reads it through `active_plugin::<AuthSession>()`.
  The router does not see `AuthSession`.
- **Sign-out → guard re-evaluation (§5.10.6)**: the auth plugin
  MUST call `router::reevaluate_current()` when `AuthSession`
  transitions identity (sign-in, sign-out, refresh). The router
  re-runs guards on the currently-painted route; gated
  components are unmounted (dropping their `LoaderSlot`) before
  the new outcome paints. Without this, a signed-out user's
  previous PII stays on screen until they happen to navigate.
- **Session-epoch ownership (§5.10.5)**: `AuthSession` exposes a
  monotonic `u64` epoch that bumps on every identity change. The
  auth fetch middleware captures the epoch on outgoing requests
  and checks it on dispatch/response so post-sign-out responses
  do not leak.

That's the full integration surface. Everything else is generic
router and fetch infrastructure.

### 5.10 Security model

This section names the trust boundaries this RFC introduces and the
contracts implementations must honor. Items here are **not optional
nice-to-haves** — they are conditions on the spec. Reviewers
should reject implementations that violate any of §5.10.1–§5.10.7.

#### 5.10.1 Client guards are UX, not authorization

Route guards prevent paint and reduce flicker. They are **not** the
security boundary. Every wasm bundle, route registration, and
`AuthSession` value is under the user's control: a determined attacker
can edit their JWT in `localStorage` to add `roles: ["admin"]`,
patch the wasm binary to short-circuit a guard's `decide` to
`Allow`, or call a `#[server]` function directly with `curl`.

The security boundary is the server. Every `#[server]` function that
touches sensitive data **MUST** carry its own `#[server(guard =
…)]` policy. The same `Predicate` value works in both sites — the
symmetry is intentional, and the server-side check is the only one
an attacker cannot bypass.

The framework actively encourages the correct symmetry (one
predicate, two install points; same `Decision` type) but cannot
prevent misuse. Implementations of this RFC **MUST** state this
prominently in the public docs (`docs/route-guards-and-loaders.md`)
and the rustdoc on `RouteConfig::guard`. Reviewers of consuming
PRs should treat "I added a client guard, the route is secure" as a
defect.

#### 5.10.2 Extension redirect intent: path-only validation

The router does not know about login, but it does provide an opaque
`ReturnTo` value for plugins that want to preserve route intent. Any
extension that turns a rejection into a redirect with a return target
is on the open-redirect path: a victim can arrive at
`/login?redirect=https://evil.com/`, sign in, and be bounced to the
attacker's site unless the value is constrained.

The router **MUST** validate the current path before constructing
`ReturnTo`, and every plugin that accepts a user-provided return
target **MUST** re-run the same validation before navigating. The
validation rule:

1. After percent-decoding, the value MUST start with exactly one
   forward slash followed by a non-slash character — regex
   `^/[^/].*$` semantics. Values starting with `//` (protocol-
   relative), `/\` (Windows-style), `\\`, or with any colon before
   the first `/` (`javascript:`, `data:`, `mailto:`, etc.) are
   rejected.
2. The value MUST NOT contain control characters (U+0000 through
   U+001F, plus U+007F).
3. After decoding, rule 1 applies **again** to defeat double-
   encoding bypasses (`%2F%2Fevil.com` → `//evil.com`).
4. Optional and recommended: the value MUST match a registered
   route pattern. Auth plugins expose this as
   `return_to_validation(ReturnToValidation::RegisteredRoutes)`;
   the default is the path-only check above.

Rejected values produce `ReturnTo::none()` and plugins redirect
without a return parameter. The router does not surface a "tried to
redirect off-origin" error to the user; the loader/guard outcome
simply produces a redirect without intent.

The path is captured **at intent-build time** by the router (via
`window.location.pathname + window.location.search`), not parsed from
query strings at consume time. So a `?redirect=…` appended by an
attacker to `/login` itself is irrelevant to the router-built intent
contract. The auth plugin's login surface MAY trust a redirect
parameter it produced from `ReturnTo`; if the surface also accepts
user-typed or externally supplied redirect values, it MUST re-run the
validation above.

URL-encoding in the produced URL uses
`web_sys::UrlSearchParams::set` (or the equivalent percent-encoding
that escapes the RFC 3986 §2.2 reserved set in the query-value
position). The consumer reads via `UrlSearchParams::get`. Both
halves use the same encoding contract or the round trip breaks.

#### 5.10.3 Fetch middleware is trusted-plugin code

Middlewares observe and can mutate every outgoing `#[server]`
request and response. They can read request bodies (which carry
PII or unredacted credentials), synthesize fake `Ok` responses
that components will trust, suppress `Err(Unauthorized)` to hide
auth failures, or replay requests. This is **intentional** —
middleware is the seam auth plugins use to make 401 handling
transparent — but it places middleware on the trusted-code path.

Implementation contract:

- `fetch::install_middleware` **MUST** freeze the chain at the
  first `App::run` (or first `fetch::call`, whichever comes
  first). Calls afterwards panic with a diagnostic naming the
  offending plugin. Hot-reloading middleware is not supported
  and would defeat the trust boundary.
- Apps **SHOULD** treat `fetch::install_middleware` as a
  privileged install API on par with `App::plugin`. Untrusted
  dependencies should not be granted middleware install rights;
  reviewers of PRs that add a `fetch::install_middleware` call
  should treat it the way they'd treat a new SQL query.
- Telemetry middleware **MUST** default to redacted metadata
  only: function name (the macro-generated `&'static str`), HTTP
  status, duration, error class, optional payload size in bytes.
  Request and response bodies, headers, query strings, and
  cookies are **never** part of the default exporter payload;
  apps explicitly opt in to body capture via the telemetry
  plugin's own configuration.
- Auth middleware that retries on Unauthorized **MUST** follow
  the replay contract in §5.10.4.

#### 5.10.4 Replay safety after Unauthorized

`fetch::call` POSTs the call body. POST is non-idempotent: a
server function that partially completed before returning 401
has already taken effect. Naive "replay after refresh" turns a
single client click into two server-side mutations — a duplicate
charge, a double-sent email, a doubled inventory decrement.

The auth plugin's middleware **MUST** follow these rules:

1. **Single-flight refresh.** When N parallel requests fail with
   `Unauthorized`, the middleware refreshes the token **once**;
   the N-1 other failed requests share the same refresh result
   and replay only after the single refresh resolves. Two
   simultaneous Unauthorized responses MUST NOT trigger two
   refresh round trips.
2. **At most one replay per request.** The replayed request
   **MUST NOT** trigger another refresh on its own
   `Unauthorized` — that's a sign refresh produced an invalid
   token; delegate to the auth plugin's configured rejection UX.
   A second 401 on the replay surfaces to the loader as
   `Err(Unauthorized)` unchanged.
3. **Replay-safe gate.** Replay only fires for requests the
   framework knows are safe to retry. The default safe set:
   - `#[server]` functions explicitly marked
     `#[server(idempotent)]`. RFC-078 requires RFC-066 (server-
     function auth and access policy) to add this attribute as a
     follow-up; until it does, the replay-safe set is **empty**
     and the auth middleware MUST propagate the original
     `Err(Unauthorized)` upward without retry. (This is a
     deliberate fail-closed default.)
   - GET-style server functions, when RFC-066 grows them.
   For non-replay-safe requests, the middleware does **NOT**
   replay; it propagates `Err(Unauthorized)` upward, the loader
   catches `LoaderError::Unauthorized`, and the router delegates
   `RouteRejection::Unauthorized` to the extension chain. The
   active extension decides the recovery UX. This is correct
   behaviour, not a UX regression: the
   alternative — silent server-side double-mutation — is worse.
4. **Idempotency keys** are an alternative to
   `#[server(idempotent)]` for request-scoped uniqueness —
   middleware can attach a client-generated key and the server
   deduplicates. This is opt-in per-request and out of scope for
   this RFC; the framework primitive is the safe-vs-unsafe
   distinction.

Applications that want broader retry semantics can install a
custom retry middleware **after** the auth middleware in the
chain, with explicit awareness that the requests being retried
may be non-idempotent. The framework's auth retry is the
conservative default.

#### 5.10.5 Cancellation: actual abort, not just drop

§5.8's `RouteToken` check drops the loader's *result*, but the
underlying request is still in flight: the server still
processes it, the `Authorization` header was already sent, and
the response is swallowed by the cancelled loader. For a logged-
out user, this means in-flight requests can complete with the
former identity's credentials.

Real abort semantics:

- `LoaderContext` **MUST** expose a cancellation handle (an
  `AbortSignal` backed by `web_sys::AbortController`) that
  loaders pass into `fetch::call`.
- `FetchRequest` **MUST** carry an optional `AbortSignal`; when
  the signal is triggered, the underlying `window.fetch` is
  aborted via the standard browser API. Middleware in flight at
  abort time receives an `Err(ServerError::Network(_))`
  identifying cancellation.
- The router **MUST** trigger the abort when navigation
  supersedes the loader, **before** it bumps `RouteToken`.
- The auth plugin **SHOULD** maintain a session epoch (a
  monotonic `u64` bumped on sign-in, sign-out, and refresh).
  The auth middleware captures the epoch at request-build time
  and re-checks it at dispatch and at response time; on
  mismatch, the request is aborted before dispatch (its
  `Authorization` would carry stale identity) and the response
  is dropped after dispatch (its body may have been computed
  for a different identity). The epoch lives on `AuthSession`;
  the abort plumbing is the auth plugin's responsibility.

This makes "user signed out while a loader was in flight" a
routine no-op rather than a credential leak.

#### 5.10.6 Sign-out triggers guard re-evaluation

When `AuthSession` transitions from "signed in" to a different
identity (or to "signed out"), the auth plugin **MUST** trigger
re-evaluation of route guards on the currently-painted route.
If a guard now returns `Redirect` or `Reject`, the router unmounts
the current component (dropping its `LoaderSlot` and any in-memory
data) before applying the new outcome.

This prevents a stale-PII bug:

1. User A signs in, navigates to `/dashboard`, sees their data.
2. User A clicks "sign out"; `AuthSession` is cleared.
3. Without re-evaluation, the dashboard component is still
   mounted with A's data.
4. User B walks up to the same browser; without navigating, B
   sees A's data.

With re-evaluation, sign-out unmounts gated components
synchronously: `require_auth().decide()` sees no principal,
returns `Reject(RouteRejection::Unauthorized)`, the router unmounts
the dashboard, and the auth plugin's route-rejection handler decides
whether B sees the login route, a modal login, an external IdP
handoff, or another configured auth surface.

The router exposes a re-evaluation hook (`router::reevaluate
_current()`) that auth plugins call when their session changes.
The plugin **MUST** wire it; the contract is on the plugin, not
on the application. The router does not impose this — it
provides the primitive — but a `pocopine-auth-client` plugin
that ships without it is rejected at review.

#### 5.10.7 LoaderError: default UI is generic; events use stable reasons

`LoaderError::Forbidden(String)`, `NotFound(String)`, and
`Server(ServerError)` carry messages so apps can render
contextual error surfaces if they want. The framework defaults
**MUST NOT** display these messages.

- The default `route_error_component` and `not_found_component`
  show a small **generic** message ("This page is unavailable.",
  "Page not found.") with no error string interpolated. Apps
  override these with their own components if they want richer
  content; until they do, no server-internal error string
  reaches the painted DOM.
- `RouteNavigationFailed` events **MUST** carry only stable
  reason identifiers from a closed set:
  - `"loader_unauthorized"`
  - `"loader_forbidden"`
  - `"loader_not_found"`
  - `"loader_server_error"`
  - `"guard_unauthorized"`
  - `"guard_forbidden"`
  - `"guard_blocked"`
  - `"guard_redirected"`
  - `"guard_rejected"`
  The error message strings never enter the framework event;
  observability plugins that want them must read from the
  underlying `tracing` record (which already follows the
  redaction rules from RFC-077 §6).

This prevents accidental disclosure of server-internal error
messages (stack traces, query strings, internal paths, error
descriptions like "user 4711 not found in shard us-east-2")
through the default UI or through plugin event consumers.

## 6. Phased Plan

### Phase 1 — Generic route primitives + auth `Predicate`

- Add `RouteGuard` trait, `RouteGuardDecision` enum, and the
  `RouteContext` carrying `path`, `params`, `query`,
  `matched_pattern`, and validated `return_to()` helper to
  `pocopine-core::router`. Router-only; no auth deps.
- Add `RouteRejection`, `RouteRejectionHandler`,
  `RouteRejectionAction`, and `ReturnTo` to
  `pocopine-core::router`.
- Add `App::route_rejection_handler` as a generic extension point.
  No `App::login_route`, `App::login_redirect_param`, or other
  auth-shaped app-shell methods.
- Add `Predicate` trait, `Decision` enum, and the standard
  predicates (`require_auth`, `require_role`,
  `require_permission`, `any_of`, `all_of`) to `pocopine-auth`.
  Shared crate; no router deps.
- Add `From<Decision> for Result<(), ServerError>` adapter so
  `#[server(guard = …)]` accepts predicates server-side.
- (Client-side `Predicate`-as-`RouteGuard` blanket impl ships in
  `pocopine-auth-client` later, not in this RFC.)

### Phase 2 — Client route guards + rejection extension dispatch

- Add `RouteComponent` and `RouteConfig<C>`.
- `App::route::<C: RouteComponent>(...)` calls
  `<C as RouteComponent>::config()`.
- `RouteConfig::guard(impl RouteGuard)` records the guard.
- Router runs guards in registration order; `Allow`/`Redirect`/`Reject`
  outcomes drive navigation/extension dispatch.
- Add `RouteNavigationFailed` events for guard outcomes using the
  closed-set reason identifiers in §5.10.7 (`"guard_redirected"`,
  `"guard_unauthorized"`, `"guard_forbidden"`, etc.).
- `app!{}` keeps path/component route entries and uses each
  component's `RouteComponent::config()`; inline guard syntax is
  deferred unless a real use case beats the component-local shape.
- `ReturnTo` plus the §5.10.2 validation rules (path-only check,
  double-decode, control-char reject, optional
  `ReturnToValidation::RegisteredRoutes`). Tests cover `https://`,
  `//`, `/\`, `javascript:`, `data:`, `%2F%2Fevil`, control
  characters.
- `router::reevaluate_current()` primitive (§5.10.6).

### Phase 3 — Client route loaders + abort plumbing

- `RouteConfig::loader(...)` records a single async loader (panic
  on second registration for the same route).
- Router runs the loader after guards, before mount.
- `LoaderError` enum + `From<ServerError>` impl.
- `Loader<T>` extractor wired into `LifecycleContext`; per-mount
  lifetime, no cache.
- **Real cancellation** (§5.10.5): `LoaderContext` exposes an
  `AbortSignal`; `FetchRequest` carries an optional
  `AbortSignal`; the router aborts the controller before bumping
  `RouteToken` on supersession.
- `App::not_found_component` / `App::route_error_component`
  generic fallback builders. Defaults paint **generic** error copy
  (§5.10.7).
- `pocopine-auth-client` installs its auth-owned rejection handler
  via `AppPlugin`: login route, return-intent parameter, validation
  strictness, modal/external-provider variants, and post-login
  policy all live on the plugin builder.

### Phase 4 — Fetch middleware chain + replay contract

- `pocopine_core::fetch::install_middleware`. Chain freezes at
  first `App::run` / `fetch::call`; later install panics
  (§5.10.3).
- Macro-generated `#[server]` stub passes through the chain.
- Middleware contract: `Result<FetchResponse, ServerError>`, `Err`
  short-circuits.
- `#[server(idempotent)]` attribute (RFC-066 follow-up) so the
  framework knows which requests are replay-safe (§5.10.4).
  Until that attribute lands, the auth replay-safe set is empty
  and Unauthorized always propagates.
- `RouteNavigationFailed` events use the closed-set reasons
  from §5.10.7.

### Phase 5 — Documentation + integration tests

- `docs/route-guards-and-loaders.md` walking through the four
  phases AND the security model with a "client guards are not
  authorization" callout at the top.
- wasm tests covering: guard outcomes; loader success/failure
  paths; fetch middleware ordering; the `Unauthorized`
  route-rejection flow; the auth-plugin return-intent round trip
  including every rejection case from §5.10.2; abort propagation
  (request cancelled when navigation supersedes); session-epoch
  rejection on post-sign-out responses; replay-safe vs
  not-replay-safe Unauthorized behavior.
- Update RFC-074 / RFC-076 to point at this RFC for the
  `Predicate` trait and the `RouteGuard` primitive.

## 7. Privacy and reliability

The full security contract is in §5.10. This section recaps the
non-security reliability properties; security-relevant items are
tagged with the §5.10 subsection that owns the contract.

- Guards are sync and side-effect-free; the router runs them on the
  navigation thread without yielding.
- Loaders run with the same `tracing` / observability surface that
  components have; `RouteNavigationStarted` already fires before the
  loader, `RouteNavigationCompleted` fires after the component
  mounts.
- Fetch middleware **MUST NOT** log request or response bodies in
  default exporter payloads (§5.10.3). The redaction default is
  metadata-only: function name, status, duration, error class,
  optional payload size.
- Loader errors **MUST NOT** leak server-side error strings into
  the redirect URL or the painted error surface (§5.10.7). The
  default error components show generic copy; apps that want
  contextual messages override via
  `App::route_error_component`/`not_found_component`.
- `RouteNavigationFailed` events carry only stable `reason`
  identifiers from a closed set (§5.10.7); message strings stay
  in `tracing` records that already follow RFC-077 §6's
  redaction rules.
- Cancellation is real: aborted requests stop on the wire
  (§5.10.5), so a logged-out user's outstanding loader does not
  complete and leak data into a stale `LoaderSlot`.

## 8. Resolved questions

The original ten open questions were resolved by council review on
2026-05-07, then revised after extensibility review. Each decision is
recorded here and reflected in the body of the RFC. The biggest
structural changes are: separating the generic `RouteGuard`
primitive from auth's `Predicate`, and moving login/return-intent UX
out of core `App` methods into plugin-owned route-rejection
extensions. Captured in §5.1 / §5.1.1 / §5.1.2 / §5.1.3 / §5.6.

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
   auth-aware. Reflected in §5.1, §5.1.2, §5.1.3, §5.9.

3. **`LoaderError::Unauthorized` semantics — generic.** The variant
   is a routing state ("this route cannot be entered without an
   extension deciding what to do"), not an auth implementation
   detail. The auth plugin produces it via fetch middleware; the
   router only knows the variant and delegates it to the
   route-rejection extension chain. Reflected in §5.3 and §5.6.

4. **Loader data lifetime — per-mount, no cache in v1.** The
   `LoaderSlot<C>` is created on `Ok(data)` and dropped on unmount.
   Re-navigating re-runs the loader. Caching/SWR/revalidation ships
   later as a plugin or explicit route option; v1 keeps the router
   free of cache semantics. Reflected in §5.4.

5. **Loader composition — one loader per route.** A second
   `RouteConfig::loader(...)` call panics at config time. Multiple
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

7. **Macro guard syntax — no inline guard syntax in v1.**
   `RouteComponent::config()` is the primary authoring point for
   guards and loaders, so `app!{}` route entries stay path/component
   pairs. Inline macro policy can be revisited later if a concrete
   use case is not served by `RouteConfig`. Reflected in §5.7.

8. **Default error components — minimal defaults ship.** Apps work
   out of the box without forcing error-component configuration. The
   defaults are intentionally plain so it's obvious in production
   that an override hasn't happened yet. Reflected in §5.6.

9. **Redirect with intent — yes, but extension-owned.** The router
   builds and validates an opaque `ReturnTo` value. It does not know
   about login routes or query parameter names. The auth plugin may
   append `ReturnTo` to its configured login route as
   `?redirect=<path>`, use `?next=<path>`, ignore it, show a modal,
   or start an external IdP flow. Reflected in §5.6 and §5.10.2.

10. **Server-loader symmetry — no new server primitive.** `#[server]`
    is already the server-side data function. Route loaders are a
    client/router orchestration around `#[server]` calls, not a new
    server-side concept. Adding a `#[loader]` macro would overload
    `#[server]`'s role and split the Rust-function-on-the-server
    surface for no clear gain. Reflected in §4 (non-goals).
