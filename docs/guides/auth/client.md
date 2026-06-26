---
title: "Wasm-side auth"
description: "Wasm-side auth: the bearer fetch middleware, AuthSession install, and predicate-based route guards."
---

# Wasm-side auth — `pocopine-auth-client`

The wasm-side companion to the credentials core and external IdP
integrations (Firebase, Clerk, Auth0, app-issued JWTs, etc.). Eight
surfaces:

- **Token slot.** `set_token` / `clear_token` / `active_token`
  manage a process-global `Option<String>` the bearer middleware
  reads at fetch time.
- **Bearer fetch middleware.** `BearerMiddleware` implements
  `pocopine_core::fetch::FetchMiddleware`; `install()` registers it
  on the global RFC-078 chain. From then on every outgoing
  `#[server]` call gets `Authorization: Bearer <token>` when a
  token is set. The middleware also enforces an identity-change
  fence: if the user signs in/out while a request is in flight,
  the response is dropped with `Unauthorized("session_changed")`.
- **Token refresh + replay (single-flight).** Apps install a
  `TokenRefresh` via `with_token_refresh(...)`. On `Unauthorized`
  for a `#[server(idempotent)]` request the middleware refreshes
  once and replays. Concurrent in-flight 401s coalesce into a
  single refresh call to the issuer.
- **Token persistence (`TokenStorage`).** Pluggable storage so the
  token survives reload. Provided impls: `LocalStorage`,
  `SessionStorage`, `InMemory` (default no-op).
- **Session snapshot persistence (`SessionSnapshotStorage`).**
  Optional optimistic identity snapshot storage for instant reload
  continuity. Snapshots are UI continuity hints, not authorization
  proof.
- **Cross-tab session coordination.** Sign-in/out in one tab
  propagates to peer tabs of the same origin via `BroadcastChannel`.
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
[`auth-credentials.md`](credentials.md). Read both — most
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
2. `app.provide_plugin(session)` where `session` is chosen by the
   builder: `AuthSession::restoring(snapshot)` when a snapshot store
   contains an authenticated principal, `AuthSession::pending()` when
   `.wait_for_initial_auth_check(true)` was set or a persisted token
   was found in storage (so the app can confirm the identity before
   routing), and `AuthSession::new()` otherwise. Any component can
   extract it as `Plugin<AuthSession>` /
   `Option<Plugin<AuthSession>>`. With
   `.wait_for_initial_auth_check(true)`, the session starts pending
   and `predicate_guard(...)` pauses navigation until the provider
   publishes the first auth result.
3. `app.route_rejection_handler(handler)` where the handler maps
   `RouteRejection::Unauthorized` to a redirect to the configured
   login route, optionally appending the validated `ReturnTo`
   intent under your chosen query param.

If the builder is never installed, none of this happens — apps
that handle auth differently (a modal-only flow, an external IdP
redirect with no return-to, an SSR-cookie-only flow) skip
`auth_plugin()` entirely and ship their own
`RouteRejectionHandler`.

For no-SSR apps backed by browser auth providers such as Firebase,
enable the first-check gate:

```rust
auth_plugin()
    .wait_for_initial_auth_check(true)
```

Then, once the provider has checked its local session, call
`AuthSession::sign_in`, `AuthSession::sign_out`,
`AuthSession::set_principal`, or `AuthSession::mark_ready`. The router
will re-run any guard that returned pending and continue with the
correct signed-in or signed-out state.

If you also want refreshes to feel native, add a snapshot store:

```rust
auth_plugin()
    .wait_for_initial_auth_check(true)
    .with_session_snapshot(LocalStorage::new("auth_snapshot"))
```

On the next page load the session starts in a restoring state from
that snapshot, so guarded route shells can render immediately while
Firebase, `/me`, or your provider SDK confirms the real session in
the background.

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

`Decision::Allow` → guard `Allow`. `Decision::Deny(DenyReason::Unauthorized)`
(the "user is not signed in" branch the standard predicates emit)
→ `RouteGuardDecision::Reject(RouteRejection::Unauthorized)`,
which the plugin's rejection handler translates into a redirect
to the login route. `Decision::Deny(DenyReason::Forbidden)` or
`Decision::Deny(DenyReason::Custom(reason))` → `Reject(Forbidden(reason.as_str()))`,
which falls through to the next handler in the chain or to the
generic error surface (see
[`route-guards-and-loaders.md`](../routing/route-guards-and-loaders.md)).

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
free-standing `active_session()` accessor:

```rust
fn on_sign_out(&self) {
    if let Some(session) = pocopine_auth_client::active_session() {
        session.sign_out();            // clears bearer slot, resets
                                       // principal, re-evaluates guards
        pocopine::navigate("/login");  // explicit redirect
                                       // (or rely on the guard's
                                       // Unauthorized redirect)
    }
}
```

`AuthSession::sign_out()` calls `reevaluate_current()` internally,
so gated routes are re-checked immediately. The explicit call in the
snippet above is redundant — include it only if you call `clear_token()`
or `set_principal()` directly rather than going through `sign_out()`.

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
use pocopine_codec::base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
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
want to re-derive on the client.

The guard confirms authentication. If the body only needs a full
user record from your auth layer, fetch it there. If it needs the
request principal or other middleware context, accept the general
`pocopine::server::RequestContext` as a server-supplied parameter and
read auth values through `RequestAuthExt`.

```rust
use pocopine_auth::{AuthUser, RequestAuthExt, require_login};
use pocopine_core::ServerResult;
use pocopine_core::server::RequestContext;

#[pocopine::server(guard = require_login)]
pub async fn me(ctx: RequestContext) -> ServerResult<AuthUser> {
    // The guard confirmed the caller is authenticated.
    // RequestAuthExt reads the auth principal/user from request extensions.
    let user = ctx.require_user()?.clone();
    Ok(user)
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
`#[server(guard = …)]` policy:

```rust
use pocopine_auth::{require_admin, require_role};
use pocopine_auth_client::predicate_guard;
use pocopine_core::{RouteComponent, RouteConfig, ServerResult};
use pocopine_core::server::RequestContext;

// Client-side: predicate_guard wraps the predicate into a RouteGuard.
impl RouteComponent for AdminPanel {
    fn config() -> RouteConfig<Self> {
        RouteConfig::new().guard(predicate_guard(require_role("admin")))
    }
}

// Server-side: guard = an async fn(RequestContext) -> ServerResult<()>.
// `require_admin` from `pocopine_auth` is a built-in for the admin role.
#[pocopine::server(guard = require_admin)]
pub async fn admin_audit() -> ServerResult<Vec<Event>> {
    // server-side check runs even if the wasm guard was bypassed
    // ...
}
```

The `#[server(guard = …)]` attribute takes a path to an `async fn(RequestContext) -> ServerResult<()>`. `pocopine_auth` ships built-in guards for the common cases: `require_login` (any authenticated user), `require_admin` (admin role), and `require_staff` (staff role). For custom role/permission checks, write a small async wrapper:

```rust
use pocopine_auth::{Predicate, RequestAuthExt, require_role};
use pocopine_core::server::RequestContext;
use pocopine_core::ServerResult;

async fn require_auditor(ctx: RequestContext) -> ServerResult<()> {
    require_role("auditor").check(&ctx.principal()).into()
}

#[pocopine::server(guard = require_auditor)]
pub async fn audit_log() -> ServerResult<Vec<AuditEntry>> {
    // ...
}
```

Server function bodies can also accept
`pocopine_core::server::RequestContext`,
`pocopine_core::server::Extension<T>`, or
`Option<pocopine_core::server::Extension<T>>` parameters. Those
parameters are filled from host request metadata and extensions, and
are omitted from the generated client stub. See
[`Server functions`](../server/server-functions.md) for the full
extractor shape.

## Step 5 — keep users signed in across reloads (`TokenStorage`)

By default the token slot is in-memory only. Refresh the page and
the user is signed out. Real apps persist the token through some
browser surface; pocopine ships a `TokenStorage` trait + provided
impls so you don't hand-roll that.

### Pick a storage backend

| Impl | Survives reload | Survives tab close | XSS-readable |
|---|---|---|---|
| `InMemory` (default) | no | no | no |
| `SessionStorage` | yes | no (per-tab) | yes |
| `LocalStorage` | yes | yes (cross-tab) | yes |
| `httpOnly` cookie (server-side) | yes | yes | **no** |

If your access tokens are short-lived bearer JWTs and you accept
the XSS exposure, `LocalStorage` is the standard choice. For
high-value applications, prefer an `httpOnly` cookie issued by the
server — pocopine then doesn't need a token slot at all (the
browser sends the cookie automatically; skip the bearer middleware).

### Wire it in

```rust
use pocopine_auth_client::{auth_plugin, storage::LocalStorage};

App::new()
    .plugin(
        auth_plugin()
            .login_route("/login")
            .with_bearer_middleware(true)
            .with_token_storage(LocalStorage::new("auth_token")),
    )
    .run();
```

That's it. Three things change:

1. At plugin install time, the in-memory token slot is hydrated from
   storage (`hydrate_from_storage()` runs). Returning users keep
   their credential, but the `Principal` still needs provider/server
   confirmation unless you also configure a session snapshot.
2. `set_token` writes through to storage on every call.
3. `clear_token` / `AuthSession::sign_out` clear storage too.

### Optional session snapshot

Token storage keeps a credential. Snapshot storage keeps a small
serialized `Principal` for optimistic UI restore:

```rust
auth_plugin()
    .with_token_storage(LocalStorage::new("auth_token"))
    .with_session_snapshot(LocalStorage::new("auth_snapshot"))
    .wait_for_initial_auth_check(true)
```

When a snapshot exists, `AuthSession` starts as `restoring`: guards
can render the route shell from the cached principal, but the session
is not considered fully confirmed until your provider calls
`sign_in`, `sign_out`, `set_principal`, or `mark_ready`. Treat the
snapshot as display continuity only; server functions remain the
authorization boundary.

### Custom storage

The trait is small. Implement it for a cookie writer, IndexedDB
adapter, native iOS keychain via a JS bridge, etc.:

```rust
use pocopine_auth_client::TokenStorage;

struct MyKeychainStorage;

impl TokenStorage for MyKeychainStorage {
    fn load(&self) -> Option<String> {
        // call into your bridge
        Some(call_native_keychain_get())
    }
    fn save(&self, token: &str) {
        call_native_keychain_set(token);
    }
    fn clear(&self) {
        call_native_keychain_clear();
    }
}
```

Plug it into the builder the same way as the provided impls — the
`with_token_storage` method takes any `TokenStorage`:

```rust
auth_plugin()
    .login_route("/login")
    .with_bearer_middleware(true)
    .with_token_storage(MyKeychainStorage)  // ← pass your impl by value
```

For storage that needs runtime configuration (e.g., a
keychain-service name read from app state), give your impl
fields and construct accordingly:

```rust
struct MyKeychainStorage {
    service: String,
}

impl MyKeychainStorage {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }
}

// Usage:
auth_plugin()
    .with_token_storage(MyKeychainStorage::new("com.example.app.auth"))
```

The plugin builder wraps your impl in an `Rc<dyn TokenStorage>`
internally, so the impl doesn't need to be `Clone` or `Send`.

## Step 6 — sync sign-in/out across tabs (`BroadcastChannel`)

Without coordination, signing in on tab A leaves tab B oblivious:
tab B keeps issuing requests under the previous identity until it
catches a 401. Worse for sign-out: tab B keeps rendering the
authenticated dashboard with stale data.

Enable cross-tab sync at builder time:

```rust
auth_plugin()
    .login_route("/login")
    .with_bearer_middleware(true)
    .with_token_storage(LocalStorage::new("auth_token"))  // required
    .with_cross_tab_sync(true)
```

**`with_token_storage` is required** for cross-tab sync to work
end-to-end. The broadcast tells peer tabs "something changed" but
doesn't carry the new token over the channel — peers re-read it
from shared storage. Calling `with_cross_tab_sync(true)` without a
configured `TokenStorage` panics at plugin install time with a clear
diagnostic.

### What happens on the wire

1. Tab A: `session.sign_in("token-xyz", principal)`.
2. Tab A's `set_token` writes `"token-xyz"` to localStorage.
3. Tab A's `set_principal` posts a message to the
   `"pocopine-auth"` `BroadcastChannel`.
4. Tab B's listener fires:
   - Calls `hydrate_from_storage()` → reads `"token-xyz"` into
     tab B's in-memory slot.
   - Calls `session.bump_epoch()` → tab B's bearer-middleware
     fence trips on any in-flight request still under the old
     identity, returning `Err(Unauthorized("session_changed"))`.
5. Tab B's app code reacts to the epoch bump (reactive view,
   call `/me`, navigate, etc.) — that's app-level wiring; the
   framework provides the primitive, not the UX.

Re-entrancy is handled internally: when tab B's listener runs,
the principal-set inside the listener is suppressed from
re-broadcasting. No infinite ping-pong.

### Optional: react to peer-tab events

The broadcast itself is the framework's signal. If your app needs
to do something specific on a peer-tab change (refetch profile,
flash a notification), watch the session epoch:

```rust
impl Dashboard {
    pub fn on_setup(&mut self, session: Plugin<AuthSession>) {
        // Capture the current epoch.
        let captured_epoch = session.epoch();
        // After every render, check if epoch advanced; if so,
        // re-fetch /me. (Concrete shape depends on the reactive
        // system you're using — this is illustrative.)
        // ...
    }
}
```

### Falling back when unavailable

`BroadcastChannel` ships in all evergreen browsers but is missing
in older Safari and `file://` contexts. `with_cross_tab_sync(true)`
silently degrades to a no-op when the constructor fails — apps
don't see a panic; they just don't get cross-tab coordination.

## Step 7 — refresh tokens automatically (`TokenRefresh`)

Access tokens expire. Without refresh, the user sees an
authentication error mid-session even when their identity is still
valid (the issuer would gladly hand them a new token if asked).

The bearer middleware can refresh and replay automatically when:
- The server function is marked `#[server(idempotent)]` (replay-safe).
- A `TokenRefresh` is configured.
- The original request actually carried a token.

```rust
auth_plugin()
    .login_route("/login")
    .with_bearer_middleware(true)
    .with_token_storage(LocalStorage::new("auth_token"))
    .with_token_refresh(|| async {
        // Talk to your provider. The closure can call any of:
        //   - your own #[server] function
        //   - Firebase / Clerk / Auth0 SDK
        //   - a refresh-cookie-backed endpoint
        // Return Ok(new_token) on success; Err propagates the
        // original Unauthorized to the caller.
        my_app::refresh_session_token().await
    })
```

The closure is `Fn() -> Future<Output = Result<String, ServerError>>`
— stateless, can be called repeatedly. For stateful refresh logic
(e.g. capturing a refresh-token cookie, holding a configured
issuer URL, sharing a connection with the rest of the app)
implement the `TokenRefresh` trait directly:

```rust
use pocopine_auth_client::{TokenRefresh, TokenRefreshFuture};
use pocopine_core::ServerError;
use std::rc::Rc;

struct MyRefresher {
    client: Rc<HttpClient>,
    issuer_url: String,
}

impl MyRefresher {
    pub fn new(client: Rc<HttpClient>, issuer_url: impl Into<String>) -> Self {
        Self { client, issuer_url: issuer_url.into() }
    }
}

impl TokenRefresh for MyRefresher {
    fn refresh(&self) -> TokenRefreshFuture {
        // Clone what the future needs to own; the future must be
        // 'static so it can outlive the &self borrow.
        let client = self.client.clone();
        let url = self.issuer_url.clone();
        Box::pin(async move {
            client
                .post(&url)
                .await
                .map_err(|e| ServerError::unauthorized(format!("refresh: {e}")))
        })
    }
}
```

Plug it into the builder — `with_token_refresh` accepts both
forms because of the blanket `impl<F, Fut> TokenRefresh for F`
that covers closures:

```rust
auth_plugin()
    .login_route("/login")
    .with_bearer_middleware(true)
    .with_token_refresh(MyRefresher::new(http_client.clone(), issuer))
    //  ↑ trait-impl form
```

vs. the closure form:

```rust
auth_plugin()
    .with_token_refresh(|| async {
        my_app::refresh_session_token().await
    })
    //  ↑ closure form (uses the blanket impl)
```

Both end up as `Rc<dyn TokenRefresh>` inside the plugin. Pick the
trait-impl form when you have configuration to thread through;
pick the closure form when the refresh is a one-line call into an
SDK or `#[server]` function.

### Mark idempotent server functions

The middleware **only** retries server functions you've explicitly
marked replay-safe. Non-idempotent calls (POST that creates an
order, transfer money, send an email) must NEVER be replayed —
the user might end up double-charged. Marking is opt-in:

```rust
#[pocopine::server(public, idempotent)]
async fn get_dashboard_stats() -> ServerResult<DashboardStats> {
    // GET-shaped: read-only, replay-safe
}

#[pocopine::server(public)]
async fn place_order(order: Order) -> ServerResult<OrderId> {
    // NOT marked: middleware propagates Unauthorized rather
    // than retry. The user sees the error and can re-submit
    // with intent.
}
```

This is RFC-078 §5.10.4's fail-closed default.

### Single-flight coalescing

If five concurrent requests all 401 (e.g. user came back to the
tab after a network blip), the middleware fires a **single**
refresh call to the issuer. The first 401 starts the refresh; the
others wait for that same outcome. After it resolves, all five
requests replay with the new token.

This matters because token issuers rate-limit. A naive "one
refresh per failed request" implementation exhausts the quota
during a quota-burst event.

### What if refresh fails?

Refresh failure (network error, the user's refresh token is
itself revoked, the issuer is down) propagates as
`ServerError::Unauthorized` with the refresh's own message. Your
app should handle that the same way as a fresh `Unauthorized`:
clear the session, navigate to `/login`, surface a UI message.

```rust
match user_request_result {
    Err(ServerError::Unauthorized(_)) => {
        session.sign_out();
        // The auth_plugin's rejection handler will redirect.
    }
    // ...
}
```

## Putting it all together — production-shape config

```rust
use pocopine::App;
use pocopine_auth_client::{auth_plugin, storage::LocalStorage};

App::new()
    .plugin(
        auth_plugin()
            // Where to redirect on Unauthorized:
            .login_route("/login")
            .return_to_query_param("redirect")
            // Wire the bearer middleware on the fetch chain:
            .with_bearer_middleware(true)
            // Persist the token across reloads:
            .with_token_storage(LocalStorage::new("auth_token"))
            // Sync identity changes across tabs:
            .with_cross_tab_sync(true)
            // Refresh on Unauthorized for #[server(idempotent)]:
            .with_token_refresh(|| async {
                my_app::refresh_session_token().await
            }),
    )
    .run();
```

That's the complete client-side auth picture: persisted, coordinated
across tabs, transparently refreshing, fenced against
identity-change races, and routing rejected requests to the login
flow with `ReturnTo`.

## What's deferred

- **The `pocopine::auth::client::…` umbrella re-export.**
  Deferred until the public surface stabilizes; apps `use
  pocopine_auth_client::{set_token, install, …};` directly for
  now.
- **Route-aware predicate adapters.** `predicate_guard` is
  `Principal`-only. Guards that need to inspect `params` /
  `query` (e.g. `/users/:id` requiring `principal.id == params["id"]`)
  must write the closure directly — see the docstring on
  `predicate_guard` for the pattern.

## See also

- [`auth-credentials.md`](credentials.md) — server-side
  Postgres + `sqlx` walkthrough for the routes this client talks
  to.
- [`route-guards-and-loaders.md`](../routing/route-guards-and-loaders.md) —
  full reference for `RouteGuard`, `RouteRejection`, the
  rejection handler chain, and `ReturnTo` validation.
- [`app-plugins.md`](../plugins/app-plugins.md) — the `AppPlugin` /
  `provide_plugin` / `Plugin<T>` machinery the auth plugin uses.
