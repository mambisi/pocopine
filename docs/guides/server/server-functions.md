---
title: "Server functions"
description: "Server-function access policies, guards, request context, and typed middleware extractors."
---

# Server functions

`#[server]` turns an async Rust function into a host route and a wasm
client stub. The function body runs only on the host; the wasm build
gets a typed stub that posts JSON to the generated route and decodes
`ServerResult<T>`.

```rust
use pocopine::ServerResult;

#[pocopine::server(public)]
pub async fn ping(name: String) -> ServerResult<String> {
    Ok(format!("hello {name}"))
}
```

## Access policy

Every server function should declare whether it is open or protected:

```rust
#[pocopine::server(public)]
pub async fn public_feed() -> ServerResult<Vec<Post>> {
    // intentionally open
}

#[pocopine::server(guard = pocopine::auth::require_login)]
pub async fn private_feed() -> ServerResult<Vec<Post>> {
    // runs only after the guard returns Ok(())
}
```

- `public` says the endpoint is intentionally open.
- `guard = path` runs an async guard before the request body is decoded.
- `idempotent` marks the generated request as replay-safe for client
  middleware such as auth refresh.
- `path = "/api/name"` opts into a stable public route path; without it,
  pocopine generates an internal path scoped by module path and function name.

A guard takes the general server request context, not an auth-owned
context:

```rust
use pocopine::{ServerError, ServerResult};
use pocopine::auth::{RequestAuthExt, Role};

pub async fn require_editor(
    ctx: pocopine::server::RequestContext,
) -> ServerResult<()> {
    if ctx.principal().has_role(&Role::named("editor")) {
        Ok(())
    } else {
        Err(ServerError::forbidden("editor role required"))
    }
}
```

Auth is layered on top of `pocopine::server::RequestContext` through
`RequestAuthExt`. Do not import `RequestContext` from `pocopine::auth`
or `pocopine_auth`; those crates provide auth helpers and values, not
the request carrier.

## Request context and extractors

Server function parameters come in two groups:

| Parameter shape | Host behavior | Client stub behavior |
|---|---|---|
| `RequestContext` | receives method, URI, headers, and typed request extensions | omitted |
| `Extension<T>` | clones a required typed request extension | omitted |
| `Option<Extension<T>>` | clones an optional typed request extension | omitted |
| any other owned `T` | decoded from the JSON request body | included |

That means middleware can attach values once, and every downstream
server function can read the same typed context without adding wire
arguments.

```rust
#[derive(Clone)]
pub struct TenantContext {
    pub tenant_id: String,
}

#[derive(Clone)]
pub struct FeatureContext {
    pub beta: bool,
}

#[pocopine::server(guard = pocopine::auth::require_login)]
pub async fn dashboard(
    ctx: pocopine::server::RequestContext,
    tenant: pocopine::server::Extension<TenantContext>,
    features: Option<pocopine::server::Extension<FeatureContext>>,
    dashboard_id: String,
) -> ServerResult<Dashboard> {
    use pocopine::auth::RequestAuthExt as _;

    let user = ctx.require_user()?;
    load_dashboard(
        &tenant.tenant_id,
        &user.id,
        &dashboard_id,
        features.as_ref().is_some_and(|f| f.beta),
    )
    .await
}
```

The generated client stub is:

```rust
pub async fn dashboard(dashboard_id: String) -> ServerResult<Dashboard>;
```

The `RequestContext`, `Extension<T>`, and `Option<Extension<T>>`
parameters are host-only. They are never serialized into the client
payload.

## Multiple middleware values

Request extensions are keyed by Rust type, so multiple middleware
layers can contribute independent context values:

```rust
use pocopine_server::axum::extract::Request;
use pocopine_server::axum::middleware::Next;
use pocopine_server::axum::response::Response;

async fn tenant_layer(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(TenantContext {
        tenant_id: "acme".to_owned(),
    });
    next.run(req).await
}

async fn feature_layer(mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(FeatureContext { beta: true });
    next.run(req).await
}
```

If two layers insert the same type, the later insert replaces the
earlier value. Use small newtypes when two values have the same
underlying shape:

```rust
#[derive(Clone)]
struct TenantId(String);

#[derive(Clone)]
struct WorkspaceId(String);
```

A missing required extension means its middleware is not wired up — a
server misconfiguration, not bad client input — so the request rejects with
`ServerError::App` (HTTP 500) before the handler body runs, naming the
missing type so you can find the unwired middleware. Use
`Option<Extension<T>>` for feature flags or context that is genuinely
optional.

## Auth values

The built-in auth middleware writes the authenticated principal into
request extensions. The usual handler shape is `RequestContext` plus
`RequestAuthExt`:

```rust
#[pocopine::server(guard = pocopine::auth::require_login)]
pub async fn account(
    ctx: pocopine::server::RequestContext,
) -> ServerResult<Account> {
    use pocopine::auth::RequestAuthExt as _;

    let user = ctx.require_user()?;
    load_account(&user.id).await
}
```

If you want the principal as a direct extractor, request it through the
typed extension map:

```rust
use pocopine::auth::Principal;

#[pocopine::server(guard = pocopine::auth::require_login)]
pub async fn account_summary(
    principal: pocopine::server::Extension<Principal>,
) -> ServerResult<AccountSummary> {
    let user = principal.require_user()?;
    load_summary(&user.id).await
}
```

Bare `Principal` or `AuthUser` parameters are treated as normal JSON
arguments by the macro. Use `RequestContext` or `Extension<Principal>`
when the value should come from middleware.
