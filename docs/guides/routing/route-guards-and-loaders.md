---
title: "Route guards, loaders, and fetch middleware"
description: "Client-side route guards, async loaders, fetch middleware, and the rejection chain."
---

# Route guards, loaders, and fetch middleware

RFC-078 ships three primitives the client side previously lacked:

1. **Route guards** — sync predicates that gate route paint.
2. **Route loaders** — async data-fetch functions that run before
   the component mounts.
3. **Fetch middleware** — a chain of wrappers around outgoing
   `#[server]` calls so plugins (auth retry, telemetry, request
   signing) can intercept transparently.

This doc walks through each, then shows how they compose. The
running example is an auth-aware dashboard — but everything here
is generic; auth is just the marquee consumer.

> **Security boundary:** client route guards are UX only. They
> prevent protected components from painting, but every sensitive
> `#[server]` function must still declare its own server-side guard.

## Route configuration lives with the component

Every component that wants route-local behavior implements
`RouteComponent`:

```rust
use pocopine::prelude::*;
use pocopine_auth::require_auth;
use pocopine_auth_client::predicate_guard;

#[derive(Default)]
#[component(template_inline = "<div>...</div>")]
struct Dashboard {
    user: AuthUser,
    stats: DashboardStats,
}

#[handlers]
impl Dashboard {
    pub fn on_setup(&mut self, data: Loader<DashboardData>) {
        self.user = data.user.clone();
        self.stats = data.stats.clone();
    }
}

impl RouteComponent for Dashboard {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(predicate_guard(require_auth()))
            .loader(|ctx| async move { fetch_dashboard(ctx).await })
    }
}
```

The app builder stays a small composition shell — guards and
loaders don't appear in `app!{}` or in `App::route(...).guard(...)`
chains. The `app!{}` macro and `App::route_component::<C>` resolve
to `<C as RouteComponent>::config()` automatically.

## Route guards

A `RouteGuard` is a sync predicate over `&RouteContext`:

```rust
pub trait RouteGuard: 'static {
    fn decide(&self, ctx: &RouteContext<'_>) -> RouteGuardDecision;
}

pub enum RouteGuardDecision {
    Allow,
    /// Guard cannot decide yet — an async client-side prerequisite
    /// is still hydrating. The router leaves the outlet untouched
    /// until the owner of that prerequisite calls
    /// `router::reevaluate_current()`.
    Pending,
    Reject(RouteRejection),
    Redirect(RouteTarget),
}
```

Closures of the right shape implement the trait via a blanket
impl, so most guards are one-liners:

```rust
RouteConfig::new()
    .guard(|ctx: &RouteContext| {
        if ctx.params.get("id").is_some() {
            RouteGuardDecision::Allow
        } else {
            RouteGuardDecision::Redirect(RouteTarget::path("/"))
        }
    })
```

Guards run in registration order. The first non-`Allow` outcome
wins:

| Outcome | Router behavior |
|---|---|
| `Allow` | continue to loader / mount |
| `Pending` | leave outlet untouched; wait for `reevaluate_current()` |
| `Redirect(target)` | `navigate(target)`, no mount, emit `RouteNavigationFailed` with `reason: "guard_redirected"` |
| `Reject(rejection)` | dispatch through the rejection chain (see below) |

### `Predicate` and the auth bridge

`pocopine_auth::Predicate` is a separate, smaller trait:

```rust
pub trait Predicate: Send + Sync + 'static {
    fn check(&self, principal: &Principal) -> Decision;
}
```

A single `Predicate` value plugs into **two** install points:

- **Server-side**: `#[server(guard = require_role("admin"))]` —
  `Decision::Deny(DenyReason::Unauthorized)` becomes
  `ServerError::Unauthorized`; `DenyReason::Forbidden` and
  `DenyReason::Custom(reason)` become `ServerError::Forbidden`.
  The `From<Decision>` adapter lives in `pocopine-auth`.
- **Client-side**: `pocopine-auth-client` ships
  `predicate_guard(predicate)` — same predicate value, used as a
  `RouteGuard`. It reads the reactive client `Principal` and maps
  `Decision::Deny` into `RouteGuardDecision::Reject`:

```rust
use pocopine_auth_client::predicate_guard;
use pocopine_auth::require_role;

RouteConfig::new()
    .guard(predicate_guard(require_role("admin")))
```

For guards that need access to route params or other context,
write a closure guard directly:

```rust
RouteConfig::new().guard(|ctx: &RouteContext| {
    let principal = pocopine_auth_client::active_principal();
    match require_role("admin").check(&principal) {
        Decision::Allow => RouteGuardDecision::Allow,
        Decision::Deny(DenyReason::Unauthorized) => {
            RouteGuardDecision::Reject(RouteRejection::Unauthorized)
        }
        Decision::Deny(reason) => {
            RouteGuardDecision::Reject(RouteRejection::Forbidden(reason.as_str()))
        }
    }
})
```

## Route loaders

A loader is an async closure that runs after guards Allow and
before the component mounts:

```rust
RouteConfig::new()
    .loader(|ctx: LoaderContext| async move {
        // `?` on a `#[server]` call propagates the right router
        // signal automatically thanks to `From<ServerError>`.
        let user = api::current_user().await?;
        let stats = api::dashboard_stats().await?;
        Ok(DashboardData { user, stats })
    })
```

The loader returns `Result<T, LoaderError>` for any `T: 'static`.
`LoaderError` carries the four router-recognized failure modes:

```rust
pub enum LoaderError {
    Unauthorized,
    Forbidden(String),
    NotFound(String),
    Server(ServerError),
}
```

`From<ServerError>` makes the common case ergonomic — a
`#[server]` call that returned `Unauthorized` propagates to
`LoaderError::Unauthorized` via `?`.

### Reading loader data in the component

`Loader<T>` is an extractor on `LifecycleContext` (mirrors the
`Plugin<T>` shape):

```rust
#[handlers]
impl Dashboard {
    pub fn on_setup(&mut self, data: Loader<DashboardData>) {
        self.user = data.user.clone();
        self.stats = data.stats.clone();
    }
}
```

Use `Option<Loader<T>>` for components that may also be mounted
via `App::mount_subtree` (no router, no loader). Required
`Loader<T>` panics with a clear diagnostic when the slot is empty
or the type doesn't match the loader's output.

### Loader error → rejection chain

The router converts `LoaderError` into a `RouteRejection` and
dispatches through the same chain guards use:

| LoaderError | RouteRejection |
|---|---|
| `Unauthorized` | `Unauthorized` |
| `Forbidden(_)` | `Forbidden("loader_forbidden")` |
| `NotFound(_)` | `NotFound` |
| `Server(_)` | `Server("loader_server_error")` |

The dynamic message in `Forbidden(reason)` / `NotFound(reason)`
is **dropped at the rejection boundary** per RFC-078 §5.10.7 —
rejection-chain reasons are stable closed-set identifiers,
never user-visible error strings. Apps that want to surface the
original message use a custom `route_error_component` and read
loader-provided context through reactive state.

### One loader per route

`RouteConfig::loader(...)` panics at config-build time if called
twice. Compose multiple async fetches inside one body:

```rust
.loader(|ctx| async move {
    let (user, stats, alerts) = futures::try_join!(
        api::current_user(),
        api::dashboard_stats(),
        api::alerts(),
    )?;
    Ok(DashboardData { user, stats, alerts })
})
```

This keeps the data shape the component sees explicit and avoids
the "I see one prop but two mysterious extractors" trap.

### Cancellation

Each navigation gets a monotonic `RouteToken` and an
`AbortSignal`. Loaders capture both at spawn. When a later
navigation supersedes the loader, the router aborts the controller
first, then advances the token. That gives two layers of protection:
browser `fetch` calls stop on the wire, and any stale result that
still resolves is **dropped silently** — no painting, no
`RouteNavigationFailed` event (the new navigation is healthy and
already running).

Long-running loaders can poll `LoaderContext::is_navigation_active()`
to short-circuit voluntarily. Honoring it is an optimisation;
correctness of the painted view doesn't depend on the loader
checking.

Generated `#[server]` client stubs inherit the loader's abort signal
automatically while the loader future is being polled. If a loader
starts work in a separate task, pass `ctx.abort_signal()` explicitly
through `fetch::FetchOptions` for direct `fetch::call_with_options`
usage.

## Route rejection chain

`RouteRejection` is the generic routing failure surface; the
router is **not** auth-aware. The full variant set:

```rust
pub enum RouteRejection {
    Unauthorized,
    Forbidden(&'static str),
    Blocked(&'static str),
    NotFound,
    Server(&'static str),
    Custom { reason: &'static str },
}
```

Plugins install handlers via `App::route_rejection_handler(...)`
to claim rejections they understand:

```rust
pub trait RouteRejectionHandler: 'static {
    fn handle(
        &self,
        ctx: &RouteRejectionContext<'_>,
        rejection: &RouteRejection,
    ) -> Option<RouteRejectionAction>;
}

pub enum RouteRejectionAction {
    Redirect(RouteTarget),
    Paint(RouteErrorSurface),
    AbortNavigation,
}
```

Handlers run in install order; the first to return
`Some(action)` claims the rejection. `None` falls through. If
**every** handler returns `None`, the router falls back to the
default `RouteErrorSurface` — a plain HTML banner unless the app
configured `App::route_error_component::<MyError>()`.

Example: an auth plugin's `Unauthorized` handler:

```rust
App::new()
    .route_rejection_handler(|ctx, rejection| {
        match rejection {
            RouteRejection::Unauthorized => {
                let return_to = ReturnTo::current();
                let target = return_to
                    .append_to(RouteTarget::path("/login"), "next");
                Some(RouteRejectionAction::Redirect(target))
            }
            _ => None,
        }
    })
```

## `ReturnTo` for redirect intent

`ReturnTo` captures the user's current path so
post-redirect-back-to-here works without manual URL plumbing:

```rust
let target = ReturnTo::current()
    .append_to(RouteTarget::path("/login"), "next");
// → "/login?next=%2Fdashboard%3Ftab%3Dinfo"
```

Per RFC-078 §5.10.2 the value is **path-only and strictly
validated**. Smuggled protocol-relative URLs, double-encoded
redirects, control characters, and Windows-path tricks become
`ReturnTo::none()` — a redirect target without a return param,
not an open-redirect — silently. The supported attack set
(rejected): `https://evil.com`, `//evil.com`, `/\foo`,
`javascript:`, `data:`, `%2F%2Fevil`, `%252F%252Fevil`,
control characters.

## `reevaluate_current` for sign-out

When a guard's truth source changes (typical: auth plugin
sign-in / sign-out), the auth plugin **must** call
`router::reevaluate_current()`. The router re-runs guards on the
current path:

- `Allow` → no-op, current mount stays.
- `Pending` → record the current URL and leave the outlet
  untouched. A later `reevaluate_current()` after the prerequisite
  completes will either mount the route or take the normal
  redirect/reject path.
- `Redirect / Reject` → outlet cleared **synchronously** (PII
  dropped before the rejection chain paints), then the full
  flow re-runs.

This is what prevents a stale dashboard staying on screen after
sign-out. Without the call, User A's data survives until the
next user-driven navigation.

## Configurable error / not-found components

The defaults are deliberately plain — production apps notice
they need overrides. Configure on `App`:

```rust
App::new()
    .route_error_component::<MyAppError>()
    .not_found_component::<MyAppNotFound>()
    .route::<Home>("/")
```

- **`route_error_component`** replaces the built-in HTML banner
  painted when a `RouteRejection` reaches the fallback
  (no rejection handler claimed it).
- **`not_found_component`** mounts a component when no
  registered route matches and no `*` wildcard is registered.
  Lower-friction alternative if you don't want to dedicate a
  routing slot to 404s.

The user component is mounted through the normal route-mount
path with full `#[component]` surface (template, handlers,
lifecycle).

> The rejection variant is **not** passed implicitly into the
> error component. If you want to read which rejection
> produced the paint, register a `RouteRejectionHandler` that
> captures the rejection into reactive state before falling
> through.

## Fetch middleware

`fetch::install_middleware` wraps every outgoing `#[server]`
call. Each middleware sees a `FetchRequest` and decides whether
to forward via `next.run(request)` or short-circuit with
`Err(ServerError::…)`:

```rust
fetch::install_middleware(|req: FetchRequest, next: FetchNext| async move {
    // Add an auth header.
    let mut req = req;
    if let Some(token) = current_token() {
        req.set_header("authorization", format!("Bearer {token}"));
    }
    let response = next.clone().run(req.clone()).await;
    if let Err(ServerError::Unauthorized(_)) = &response {
        // Refresh + replay only when the server function opted in
        // with #[server(idempotent)].
        if req.is_replay_safe() {
            if let Some(new_token) = refresh_token().await {
                store_token(new_token);
                return next.run(req).await;
            }
        }
    }
    response
});
```

`FetchNext: Clone` so middleware can replay; `FetchRequest:
Clone` for the same reason.

### Replay-safe requests

Generated `#[server]` stubs are **not** replay-safe by default.
Mark read-only or otherwise idempotent server functions explicitly:

```rust
#[pocopine::server(public, idempotent)]
async fn dashboard_stats() -> pocopine::ServerResult<DashboardStats> {
    // ...
}
```

The client stub marks the outgoing `FetchRequest` as
`replay_safe`. Auth middleware may replay such a request at most once
after a successful refresh. Unmarked server functions stay
fail-closed: middleware should propagate `Unauthorized` rather than
retry a POST that may have already taken effect.

### Freeze-at-boot

The chain **freezes** at the first `App::run` or first
`fetch::call`. Subsequent `install_middleware` calls panic with
a diagnostic naming the violation. Per RFC-078 §5.10.3
middleware is privileged code (sees request bodies, can
synthesize fake `Ok` responses); freezing closes the seam where
untrusted code could install itself after the trust boundary
closed.

> **Apps SHOULD treat `fetch::install_middleware` as a
> privileged install API on par with `App::plugin`.** Untrusted
> dependencies should not be granted middleware install rights.
> Reviewers of PRs that add a `fetch::install_middleware` call
> should treat it the way they'd treat a new SQL query.

## Privacy contract

A few invariants apply to every primitive in this doc — non-
negotiable per RFC-078 §5.10:

1. **Client guards are UX-only**, not security boundaries.
   Every `#[server]` touching sensitive data must have its own
   `#[server(guard = …)]`.
2. **`ReturnTo` is path-only** (no fragment, no host, no
   scheme, no query-param-scraped values).
3. **Fetch middleware is privileged code that freezes at boot.**
4. **`RouteNavigationFailed` events** carry only closed-set
   `reason` identifiers (`"guard_unauthorized"`,
   `"loader_forbidden"`, …). User-visible error message strings
   never reach observability events.
5. **Default error surfaces** show generic copy. Apps that
   want richer error UI configure `route_error_component`.
6. **Sign-out re-evaluates guards** synchronously
   (`reevaluate_current()`) so PII never survives the
   identity-change moment.
7. **Replay-after-Unauthorized is fail-closed** unless the generated
   request is marked by `#[server(idempotent)]`; auth middleware
   should propagate `Unauthorized` rather than retry POSTs that may
   have already taken effect.

## End-to-end example

```rust
use pocopine::prelude::*;
use pocopine_auth::require_auth;
use pocopine_auth_client::{auth_plugin, predicate_guard};

// ─── Component ──────────────────────────────────────────────

#[derive(Default)]
#[component(template = "Dashboard.poco")]
struct Dashboard { user: AuthUser, stats: DashboardStats }

#[handlers]
impl Dashboard {
    pub fn on_setup(&mut self, data: Loader<DashboardData>) {
        self.user = data.user.clone();
        self.stats = data.stats.clone();
    }
}

impl RouteComponent for Dashboard {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(predicate_guard(require_auth()))
            .loader(|_ctx| async move {
                let (user, stats) = futures::try_join!(
                    api::current_user(),
                    api::dashboard_stats(),
                )?;
                Ok(DashboardData { user, stats })
            })
    }
}

// ─── App ───────────────────────────────────────────────────

App::new()
    .plugin(
        auth_plugin()
            .login_route("/login")
            .with_bearer_middleware(true),
    )
    .route_component::<Dashboard>("/dashboard")
    .route_component::<Login>("/login")
    .not_found_component::<NotFound>()
    .route_error_component::<AppError>()
    .run();
```

Sign-in calls `session.sign_in(token, principal)` →
`set_principal` → `router::reevaluate_current()`. Sign-out calls
`session.sign_out()` → `router::reevaluate_current()` →
existing component unmounts before the auth handler redirects to
`/login?redirect=/dashboard`.
