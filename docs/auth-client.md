# Wasm-side auth — `pocopine-auth-client`

The wasm-side companion to the credentials core / Firebase preset /
Clerk preset / etc. Four surfaces:

- **Token slot.** `set_token` / `clear_token` / `active_token`
  manage a process-global `Option<String>` the bearer middleware
  reads at fetch time.
- **Bearer fetch middleware.** `BearerMiddleware` implements
  `pocopine_core::fetch::FetchMiddleware`; `install()` registers it
  on the global RFC-078 chain. From then on every outgoing
  `#[server]` call gets `Authorization: Bearer <token>` when a
  token is set.
- **Reactive identity.** `AuthSession` is a plugin service the
  `auth_plugin()` builder installs. Holds the active `Principal` +
  monotonic epoch; cheap to clone, internally `Rc<RefCell<…>>`.
- **Auth-aware route guards + login redirect.** `auth_plugin()` is
  an `AppPlugin` that registers a `RouteRejectionHandler` mapping
  `RouteRejection::Unauthorized` to your configured login route.
  `predicate_guard(predicate)` adapts any
  `pocopine_auth::Predicate` (`require_auth`, `require_role`,
  …) into a `RouteGuard` that reads the live `AuthSession`.

This page is the wasm-side mirror of
[`auth-credentials.md`](./auth-credentials.md). Read both — most
real apps wire them in the same change.

## At a glance

```rust
// crates/blog/src/lib.rs
use pocopine::App;
use pocopine_auth::{require_auth, require_role};
use pocopine_auth_client::{auth_plugin, predicate_guard, AuthSession};
use pocopine_core::{Plugin, RouteComponent, RouteConfig};

#[derive(Default)]
pub struct Dashboard {
    user_email: String,
}

impl RouteComponent for Dashboard {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(predicate_guard(require_auth()))
    }
}

impl Dashboard {
    pub fn on_setup(&mut self, session: Plugin<AuthSession>) {
        if let Some(user) = session.principal().user() {
            self.user_email = user.email.clone().unwrap_or_default();
        }
    }
}

#[derive(Default)]
pub struct AdminPanel;

impl RouteComponent for AdminPanel {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new()
            .guard(predicate_guard(require_role("admin")))
    }
}

#[derive(Default)]
pub struct Login;

impl RouteComponent for Login {}

pocopine::app! {
    components: [AppShell, Dashboard, AdminPanel, Login],
    plugins: [
        auth_plugin()
            .login_route("/login")
            .return_to_query_param("redirect")
            .with_bearer_middleware(true),
    ],
    routes: [
        ("/",         AppShell),
        ("/dashboard", Dashboard),
        ("/admin",    AdminPanel),
        ("/login",    Login),
    ],
}
```

## End-to-end flow

```
                ┌────────────────────────────────────────────────────────┐
   browser      │ user clicks "Sign in" on Login component               │
                │ Login posts to /_pocopine/auth/login                   │
                │ server returns {token, user}                           │
                │ Login calls AuthSession::sign_in(token, principal)     │
                │   ├── set_token(token)  ← BearerMiddleware reads this  │
                │   └── set_principal(principal) ← bumps epoch           │
                └────────────────────────────────────────────────────────┘
                                 │
                                 ▼
                ┌────────────────────────────────────────────────────────┐
                │ user navigates to /dashboard                           │
                │ router runs Dashboard's guard:                         │
                │   predicate_guard(require_auth())                      │
                │     reads active_principal()                           │
                │     → Decision::Allow                                  │
                │   guard returns Allow → mount paint                    │
                └────────────────────────────────────────────────────────┘
                                 │
                                 ▼
                ┌────────────────────────────────────────────────────────┐
                │ Dashboard.on_setup(session: Plugin<AuthSession>)       │
                │   reads session.principal().user().email               │
                └────────────────────────────────────────────────────────┘
                                 │
                                 ▼
                ┌────────────────────────────────────────────────────────┐
                │ user clicks button calling                             │
                │ #[server] my_app::dashboard_data()                     │
                │ generated wasm stub calls fetch::call(...)             │
                │   → FetchMiddleware chain runs                         │
                │   → BearerMiddleware reads active_token()              │
                │     and adds "Authorization: Bearer <token>"           │
                │   → server route's `with_auth(JwtVerifier::custom(...))` │
                │     verifies token → Principal in extensions           │
                │     → #[server(guard = require_login)] sees real user  │
                └────────────────────────────────────────────────────────┘
```

## Step 1 — install the bearer middleware

Two equivalent options:

**Option A (recommended):** let `auth_plugin()` install it for you.

```rust
auth_plugin()
    .login_route("/login")
    .with_bearer_middleware(true)
```

**Option B:** call `pocopine_auth_client::install()` yourself
(useful when you don't run `auth_plugin()` at all and only want the
bearer attach):

```rust
fn main() {
    pocopine_auth_client::install();
    // ... App::new() etc.
}
```

Either way the rule is **install before `App::run`**. The fetch
chain freezes on the first `App::run` / `fetch::call`; later
`install_middleware` calls panic with a stable diagnostic. This is
RFC-078 §5.10.3 trust-boundary policy — middleware is privileged
code that observes every outgoing request, and the framework wants
the install set fixed when boot completes. Don't try to register
new middlewares lazily.

## Step 2 — install `auth_plugin()`

The plugin builder configures three things:

```rust
auth_plugin()
    .login_route("/login")          // ← required for Unauthorized redirects
    .return_to_query_param("next")  // optional — default "redirect"
    .with_bearer_middleware(true)   // optional — default false
```

`AppPlugin::install` does:

1. Calls `pocopine_auth_client::install()` if the builder requested it.
2. `app.provide_plugin(AuthSession::new())` so any component can
   extract the session as `Plugin<AuthSession>` /
   `Option<Plugin<AuthSession>>`.
3. `app.route_rejection_handler(handler)` where the handler maps
   `RouteRejection::Unauthorized` to a redirect to the configured
   login route, optionally appending the validated `ReturnTo`
   intent under your chosen query param.

If the builder is never installed, none of this happens — apps
that handle auth differently (a modal-only flow, an external IdP
redirect with no return-to, an SSR-cookie-only flow) skip
`auth_plugin()` entirely and ship their own
`RouteRejectionHandler`.

## Step 3 — declare guarded routes

Predicates live in `pocopine_auth`. The blanket
`impl<P: Predicate> RouteGuard for P` would violate Rust's orphan
rule (both traits are foreign to `pocopine-auth-client`), so the
adaptor is a helper function:

```rust
use pocopine_auth::{require_auth, require_role, require_permission, all_of, any_of};
use pocopine_auth_client::predicate_guard;
use pocopine_core::{RouteComponent, RouteConfig};

impl RouteComponent for Dashboard {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(predicate_guard(require_auth()))
    }
}

impl RouteComponent for AdminPanel {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(predicate_guard(require_role("admin")))
    }
}

impl RouteComponent for Audit {
    fn config() -> RouteConfig<Self> {
        // Both roles must hold.
        RouteConfig::new()
            .guard(predicate_guard(all_of(
                require_role("admin"),
                require_role("auditor"),
            )))
    }
}

impl RouteComponent for Help {
    fn config() -> RouteConfig<Self> {
        // Either role holds, OR an explicit permission.
        RouteConfig::new()
            .guard(predicate_guard(any_of(
                require_role("staff"),
                require_permission("help.read"),
            )))
    }
}
```

`Decision::Allow` → guard `Allow`. `Decision::Deny("unauthorized")`
(the "user is not signed in" branch the standard predicates emit)
→ `RouteGuardDecision::Reject(RouteRejection::Unauthorized)`,
which the plugin's rejection handler translates into a redirect
to the login route. Any other `Decision::Deny(reason)` → `Reject(Forbidden(reason))`,
which falls through to the next handler in the chain or to the
generic error surface (see
[`route-guards-and-loaders.md`](./route-guards-and-loaders.md)).

## Step 4 — use `AuthSession` from components

`AuthSession` is a plugin service. Inside a component lifecycle
hook, request it as `Plugin<AuthSession>` (or `Option<Plugin<AuthSession>>`
when the component is also reusable outside a session-bearing app):

```rust
use pocopine::Component;
use pocopine_auth_client::AuthSession;
use pocopine_core::Plugin;

#[derive(Default)]
struct Dashboard {
    email: String,
}

impl Dashboard {
    fn on_setup(&mut self, session: Plugin<AuthSession>) {
        let principal = session.principal();
        if let Some(user) = principal.user() {
            self.email = user.email.clone().unwrap_or_default();
        }
    }
}
```

Outside lifecycle hooks (in event handlers, etc.) use the
component-owned accessor:

```rust
fn on_sign_out(&self) {
    if let Some(session) = self.plugins().get::<AuthSession>() {
        session.sign_out();                       // clears bearer slot,
                                                  // resets principal
        pocopine_core::reevaluate_current();      // re-runs current
                                                  // route's guards
        pocopine_core::navigate("/login");        // explicit redirect
                                                  // (or rely on the
                                                  // guard's Unauthorized)
    }
}
```

The `pocopine_core::reevaluate_current()` call is the missing
piece RFC-078 §5.10.6 calls out: when the user signs out, gated
components stay painted until the next navigation unless something
re-runs guards. Wire it into your sign-out path — typically
right after `AuthSession::sign_out()`.

For non-component code (utility modules, browser-event listeners,
…) the free-standing helpers work the same way:

```rust
use pocopine_auth_client::{active_principal, active_session};

fn breadcrumb() {
    if let Some(session) = active_session() {
        let principal = session.principal();
        // ...
    }
    // shorter form if you only need the Principal:
    let _principal = active_principal(); // anonymous if no plugin
}
```

## Sign-in flow — your typed login client

The credentials core (`pocopine-auth-credentials`) mounts
`/_pocopine/auth/{signup,login,logout}` as plain axum routes; it
doesn't ship a typed wasm client because the surface is tiny and
the call shape is identical to any other JSON POST. The simplest
shape:

```rust
use serde::{Deserialize, Serialize};
use pocopine_auth::{AuthUser, Principal};
use pocopine_auth_client::AuthSession;
use pocopine_core::{ServerError, ServerResult};

#[derive(Serialize)]
pub struct LoginRequest<'a> {
    pub email: &'a str,
    pub password: &'a str,
}

#[derive(Deserialize)]
pub struct LoginResponse {
    pub token: String,
    /// The credentials response wraps the framework's `AuthUser`
    /// shape directly: typed top-level fields (`id`, `email`,
    /// `name`, `roles`, `permissions`) plus a `claims` map for
    /// anything the app's `to_auth_user()` projected via
    /// `with_claim(...)` (e.g. `email_verified`,
    /// `phone_verified`, custom flags).
    pub user: AuthUser,
}

pub async fn login(
    session: &AuthSession,
    email: &str,
    password: &str,
) -> ServerResult<()> {
    let response: LoginResponse = pocopine_core::fetch::call(
        "/_pocopine/auth/login",
        &LoginRequest { email, password },
    )
    .await?;

    session.sign_in(response.token, Principal::from_user(response.user));
    Ok(())
}

pub async fn signup(
    session: &AuthSession,
    email: &str,
    password: &str,
) -> ServerResult<()> {
    let response: LoginResponse = pocopine_core::fetch::call(
        "/_pocopine/auth/signup",
        &LoginRequest { email, password },
    )
    .await?;
    session.sign_in(response.token, Principal::from_user(response.user));
    Ok(())
}
```

`AuthUser` deserializes directly from the response body — no
intermediate `PublicUser` shape. Custom claims live in
`response.user.claims`; reach them with `principal.user().claim("email_verified")`
once the session is signed in.

## Principal hydration with external IdPs (Firebase, Clerk, Auth0)

The credentials flow above is the easy case: the server returns
`{token, user: AuthUser}`, you have everything to build a
`Principal` immediately. With external IdPs (Firebase's SDK, Clerk's
`<SignIn>` widget, Auth0's `loginWithPopup`) you only get a token —
the SDK doesn't hand you a pocopine-shaped `AuthUser`. Two patterns:

**Decode the JWT body unverified for the wasm-side `Principal`.**
The token's signature is verified on every `#[server]` call by the
server-side middleware, so the wasm side trusting its own decoded
copy is safe — at worst, an attacker who tampers with their local
JWT gets a stale client-side `Principal` (their server requests
still fail with `Unauthorized`). The decode is small:

```rust
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use pocopine_auth::{AuthUser, Principal};

fn decode_unverified_principal(token: &str) -> Option<Principal> {
    let body = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(body).ok()?;
    let raw: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).ok()?;

    let sub = raw.get("sub")?.as_str()?.to_string();
    let mut user = AuthUser::new(sub);
    if let Some(email) = raw.get("email").and_then(|v| v.as_str()) {
        user = user.with_email(email);
    }
    if let Some(name) = raw.get("name").and_then(|v| v.as_str()) {
        user = user.with_name(name);
    }
    // Surface remaining claims so component code can read
    // provider-specific fields via `principal.user().claim("...")`.
    for (key, value) in raw {
        if matches!(key.as_str(), "sub" | "iss" | "aud" | "iat" | "exp" | "nbf" | "jti" | "email" | "name") {
            continue;
        }
        user = user.with_claim(key, value);
    }
    Some(Principal::from_user(user))
}

// Wire to Firebase's onAuthStateChanged (in your JS bridge):
#[wasm_bindgen]
pub fn __pocopine_set_token(token: String) {
    if let (Some(session), Some(principal)) =
        (pocopine_auth_client::active_session(), decode_unverified_principal(&token))
    {
        session.sign_in(token, principal);
    } else {
        pocopine_auth_client::set_token(token);
    }
}
```

**Or call a server `/me` endpoint after sign-in** to populate the
`Principal` authoritatively. Costs one round-trip; useful when the
server's `to_auth_user()` projection includes app-specific roles
(via custom Firebase claims, Clerk org metadata, etc.) you don't
want to re-derive on the client. The endpoint reads
`Principal` from the request extensions (set by the auth
middleware) and serializes the inner `AuthUser`:

```rust
#[pocopine::server(guard = require_login)]
pub async fn me() -> pocopine::ServerResult<AuthUser> {
    let principal = pocopine::server::principal()?;
    Ok(principal.user().cloned().unwrap_or_default())
}
```

Either pattern is fine. The decode-unverified path is a single
network request lighter; the `/me` path stays server-of-truth and
matches whatever roles/claims the server-side `to_auth_user()`
projection chooses to expose. Apps that don't hydrate the
client-side `Principal` end up with a signed-in `AuthSession` whose
`principal()` is anonymous — guards work for the next navigation
(after the next `/me` lookup) but immediate post-sign-in
component reads see `Principal::anonymous()`. Decode the JWT body
or call `/me`; don't skip both.

The pattern is the same regardless of provider — Firebase / Clerk /
Auth0 SDKs return their own `(token, user)` shape, you adapt it
into a `Principal`, and call `AuthSession::sign_in(token, principal)`.

## Security note — guards are UX, not authorization

Per RFC-078 §5.10.1, route guards reduce flicker and prevent
paint of pages the user shouldn't see. They are **not** the
security boundary. A determined attacker can edit their JWT in
`localStorage` to add `roles: ["admin"]`, patch the wasm binary
to short-circuit a guard's `decide` to `Allow`, or call a
`#[server]` function directly with `curl`.

The security boundary is the server. Every `#[server]` function
that touches sensitive data **must** carry its own
`#[server(guard = …)]` policy. The same `Predicate` value plugs
into both install points — the macro turns it into a
`Result<(), ServerError>` via the `From<Decision>` adapter — so
a route-guard miss never leaves a server function unprotected:

```rust
use pocopine_auth::require_role;

// Same `require_role("admin")` value works on both sides.
impl RouteComponent for AdminPanel {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(predicate_guard(require_role("admin")))
    }
}

#[pocopine::server(guard = require_role("admin"))]
pub async fn admin_audit() -> ServerResult<Vec<Event>> {
    // server-side check runs even if the wasm guard was bypassed
    // ...
}
```

## What's deferred

- **Single-flight refresh + replay-on-401.** RFC-078 §5.5 / §5.10.4.
  Depends on a `#[server(idempotent)]` attribute that doesn't
  exist yet; until then the bearer middleware doesn't retry —
  `Unauthorized` propagates upward and the loader/component
  surfaces `RouteRejection::Unauthorized` to the rejection chain.
- **Session-epoch dispatch / response gate.** RFC-078 §5.10.5.
  Will read `AuthSession::epoch()` on outgoing requests and
  abort/drop responses computed under a stale identity.
- **The `pocopine::auth::client::…` umbrella re-export.**
  Deferred until the public surface stabilizes; apps `use
  pocopine_auth_client::{set_token, install, …};` directly for
  now.

## See also

- [`auth-credentials.md`](./auth-credentials.md) — server-side
  Postgres + `sqlx` walkthrough for the routes this client talks
  to.
- [`route-guards-and-loaders.md`](./route-guards-and-loaders.md) —
  full reference for `RouteGuard`, `RouteRejection`, the
  rejection handler chain, and `ReturnTo` validation.
- [`app-plugins.md`](./app-plugins.md) — the `AppPlugin` /
  `provide_plugin` / `Plugin<T>` machinery the auth plugin uses.
