# RFC 066 - Server-function auth and access policy

| Field | Value |
|---|---|
| **Status** | Draft |
| **Author** | pocopine team |
| **Created** | 2026-05-02 |
| **Related** | [`rfc-002-app-stores-servers.md`](./rfc-002-app-stores-servers.md), [`rfc-059-server-side-rendering-and-hydration.md`](./rfc-059-server-side-rendering-and-hydration.md) |
| **Supersedes** | RFC 002's server-function auth non-goal |

## 1. Summary

Add a first-class access policy to `#[server]` functions:

```rust
#[pocopine::server(public)]
pub async fn search(query: String) -> ServerResult<Vec<Row>> {
    /* intentionally open */
}

#[pocopine::server(guard = require_user)]
pub async fn save_profile(input: ProfileInput) -> ServerResult<Profile> {
    /* only runs after require_user accepts the request */
}

pub async fn require_user(
    ctx: pocopine::auth::RequestContext,
) -> ServerResult<()> {
    let Some(token) = ctx.bearer_token() else {
        return Err(ServerError::unauthorized("missing bearer token"));
    };
    verify_token(token).await
}
```

The framework motto for server functions is: "do not let users shoot
themselves in the foot." A server function without an access policy is
not invisible framework plumbing; it is a network endpoint. Pocopine
therefore warns when an endpoint is neither explicitly public nor
guarded.

## 2. Motivation

RFC 002 deliberately deferred authentication and session middleware so
server functions could land as a small JSON-over-POST primitive. That
was the right first step, but the current default is unsafe for real
apps: adding `#[server]` creates a public route unless the author
remembers to add checks inside the function body.

That is a footgun in three ways:

- The insecure state is silent.
- The access rule is easy to miss during review.
- Client-side route guards can be mistaken for server-side access
  control.

Pocopine should make the access boundary obvious at the function
definition and enforce protected checks in the generated server route
before the user body runs.

## 3. Goals

- Every server function has a visible access policy in source:
  `public` or `guard = path`.
- Missing policy emits a compile-time warning during migration.
- Guarded functions run the guard before deserializing application
  intent into side effects.
- Guards receive request metadata needed for common auth/session
  checks: method, URI, headers, bearer token, and cookies.
- Auth failures use typed `ServerError` variants so clients can
  distinguish unauthenticated, unauthorized, application, and network
  failures.

## 4. Non-goals

- Pocopine does not choose a session store, OAuth provider, password
  hashing scheme, or database model.
- This RFC does not make client-side route guards authoritative.
- This RFC does not add CSRF, CORS, SameSite, or rate-limit policy.
  Those belong in the host server/middleware layer.
- This RFC does not make missing policy a hard error immediately.
  The migration starts as a warning so existing examples and apps can
  opt in intentionally.

## 5. Design

### 5.1 Policy syntax

`#[server]` accepts these policy forms:

```rust
#[pocopine::server(public)]
#[pocopine::server(guard = require_user)]
#[pocopine::server(guard = crate::auth::require_admin)]
#[pocopine::server(guard = "crate::auth::require_admin")]
```

`public` means the route is intentionally open. It suppresses the
missing-policy warning but does not install a guard.

`guard = path` means the route is protected. The generated Axum route
constructs a `pocopine_server::auth::RequestContext`, awaits the guard,
and only calls the original function body if the guard returns `Ok(())`.

`#[server]` with no policy still compiles in this phase, but emits:

```text
pocopine #[server] function `name` has no access policy. Write
#[server(public)] for an intentional public endpoint or
#[server(guard = path::to_guard)] to enforce auth.
```

Future phases may promote this warning to a hard error.

### 5.2 Guard contract

The initial guard contract is intentionally narrow:

```rust
pub async fn require_user(
    ctx: pocopine::auth::RequestContext,
) -> ServerResult<()> {
    /* inspect ctx, return Ok or ServerError */
}
```

The generated route accepts any guard error type that can convert into
the server function's error type. In normal pocopine code, guards return
`pocopine::ServerResult<()>`.

### 5.3 Request context

`pocopine::auth::RequestContext` and the re-exported
`pocopine_server::auth::RequestContext` contain:

- `method()`
- `uri()`
- `headers()`
- `header(name)`
- `bearer_token()`
- `cookie(name)`
- `session_id()`
- `user: Principal`

`RequestContext` and the HTTP-backed guard helpers are host-only. The
cross-target auth value types remain available to client code:
`AuthUser`, `Principal`, `Role`, `Permission`, and `Session`.

`Principal` is anonymous by default. Host middleware may validate a
session/JWT/provider token and insert either `AuthUser` or `Principal`
into Axum request extensions. The generated server route copies that
identity into `RequestContext` before running the guard.

The context does not own the request body. This keeps auth guards from
accidentally consuming the JSON payload before the server-function
argument extractor runs.

`cookie(name)` is intentionally small and meant for simple session
cookies; it does not implement full RFC 6265 quoted-value parsing.

### 5.4 Native auth core

The `pocopine-auth` crate provides the provider-neutral auth surface:

```rust
pub struct AuthUser {
    pub id: String,
    pub roles: Vec<Role>,
    pub permissions: Vec<Permission>,
}

pub enum Role {
    Admin,
    Staff,
    User,
    Named(String),
}
```

It also provides built-in path guards:

```rust
#[pocopine::server(guard = pocopine::auth::require_login)]
pub async fn dashboard() -> ServerResult<Dashboard> { /* ... */ }

#[pocopine::server(guard = pocopine::auth::require_admin)]
pub async fn admin_stats() -> ServerResult<Stats> { /* ... */ }
```

And reusable checks for custom guards:

```rust
pub async fn require_editor(ctx: pocopine::auth::RequestContext) -> ServerResult<()> {
    pocopine::auth::ensure_role(&ctx, Role::named("editor"))
}
```

Provider integrations such as Clerk/Auth0/Supabase should adapt into
`AuthUser`/`Principal` instead of changing the server-function ABI.

### 5.5 `protected!` sugar

For concise inline role checks, `protected!` emits a private guard and
then expands to `#[server(guard = generated_guard)]`:

```rust
pocopine::protected! {
    require |ctx| ctx.user.has_role(Role::Admin);

    pub async fn create_post(
        title: String,
        description: String,
    ) -> ServerResult<Post> {
        /* protected */
    }
}
```

This is syntax sugar only. The generated route still runs auth before
reading or decoding the request body. The `require |ctx| ...` closure is
a synchronous boolean check; async/provider work belongs in a named guard
function passed to `#[server(guard = path)]`.

### 5.6 Error vocabulary

`ServerError` grows:

```rust
ServerError::Unauthorized(String)
ServerError::Forbidden(String)
ServerError::BadRequest(String)
```

Use `Unauthorized` when authentication is missing or invalid. Use
`Forbidden` when the caller is authenticated but lacks permission. Use
`BadRequest` when the JSON request body is malformed or exceeds the
server-function body limit.

### 5.7 Generated route shape

For a guarded function:

```rust
router.route(
    "/_pocopine/save_profile",
    post(|request: Request| async move {
        let (parts, body) = request.into_parts();
        let ctx = RequestContext::from_parts(
            parts.method,
            parts.uri,
            parts.headers,
            parts.extensions,
        );
        if let Err(err) = require_user(ctx).await {
            return Json(Err(err.into()));
        }
        let args = match decode_json_body_with_limit(body).await {
            Ok(args) => args,
            Err(err) => return Json(Err(err)),
        };
        Json(save_profile(args).await)
    }),
)
```

This matters: access control is installed by the server route helper,
not by client code and not by convention inside the body.

Generated routes use the same explicit body limit for public and guarded
server functions. The default is 2 MiB and can be overridden with
`POCOPINE_SERVER_FUNCTION_BODY_LIMIT` (`kb`, `kib`, `mb`, and `mib`
suffixes are accepted). Zero and invalid values are rejected, log a
warning, and fall back to the default.

## 6. Migration

1. Mark intentionally open endpoints as `#[server(public)]`.
2. Add guard functions for protected endpoints and switch them to
   `#[server(guard = path)]`.
3. Keep CI warnings visible while apps migrate.
4. After examples and downstream apps have moved, make missing policy a
   hard macro error.

## 7. Open Questions

1. Should guard failures eventually return HTTP 401/403 statuses while
   still preserving typed `ServerError` decoding in the wasm client?
2. Do we need CSRF helpers in `pocopine-server`, or should those stay in
   host-framework middleware?
