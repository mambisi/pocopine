#![cfg(not(target_arch = "wasm32"))]
//! pocopine-server — host-side helpers for `#[server]` functions.
//!
//! This crate is **not** compiled for wasm32. It lives on the server
//! side. Users include it in the `[[bin]] server` target of their app
//! and import the crate's axum re-exports plus the static-file helper
//! that serves the wasm pkg produced by `wasm-pack`.
//!
//! ```no_run
//! use pocopine_server::{axum::{self, Router}, serve, static_files};
//!
//! # mod my_app {}
//! // Link the app crate once so its `#[server]` inventory entries are present.
//! use my_app as _;
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     let router = Router::new()
//!         .nest_service("/", static_files("pkg"))
//!         .merge(Router::new());
//!     serve(router, "0.0.0.0:3000").await
//! }
//! ```

pub use axum;
pub use inventory;
pub use serde_json;
pub use tokio;
pub use tower;
pub use tower_http;
pub use tracing;

// RFC-100 Mode B — env-enabled `/assets/<hash>/<path>` private-bucket
// proxy; installed automatically by `Server::new`.
mod assets;
mod observability;
pub mod plugin;
mod server;
mod server_functions;
mod sse_response;
mod static_files;

pub use assets::{
    ASSETS_ACCESS_KEY_ID_ENV, ASSETS_BUCKET_ENV, ASSETS_ENDPOINT_ENV, ASSETS_REGION_ENV,
    ASSETS_SECRET_ACCESS_KEY_ENV,
};
pub use observability::request_event_layer;
pub use plugin::{
    HttpRequestCompleted, HttpRequestFailed, HttpRequestStarted, PluginValidationError, RequestId,
    ServerBootFailed, ServerBootStarted, ServerFunctionCompleted, ServerFunctionFailed,
    ServerFunctionRejected, ServerFunctionStarted, ServerHook, ServerListening, ServerPluginHandle,
    active_plugin, emit, has_http_request_completed_hooks, has_http_request_failed_hooks,
    has_http_request_hooks, has_http_request_started_hooks, has_server_boot_failed_hooks,
    has_server_boot_started_hooks, has_server_function_completed_hooks,
    has_server_function_failed_hooks, has_server_function_hooks,
    has_server_function_rejected_hooks, has_server_function_started_hooks,
    has_server_listening_hooks, next_request_id,
};
pub use pocopine_core::server::{Extension, FromRequestContext, RequestContext};
pub use server::{Server, ServerPlugin};
pub use server_functions::{
    ServerFunctionRoute, ServerFunctionRouteConflict, install_server_functions,
};
pub use sse_response::{sse_error_response, sse_stream_response};
pub use static_files::{StaticFiles, index_file};

#[doc(hidden)]
pub use plugin::__reset_for_test;

use std::sync::Arc;
use std::sync::OnceLock;

use axum::Router;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use pocopine_auth::{AuthProvider, Principal};

/// Default maximum JSON request body accepted by generated
/// server-function routes.
pub const DEFAULT_SERVER_FUNCTION_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Auth contracts passed to `#[server(guard = ...)]` functions.
pub mod auth {
    pub use pocopine_auth::*;
}

/// Server-function body limit in bytes.
///
/// Set `POCOPINE_SERVER_FUNCTION_BODY_LIMIT` to override the default.
/// Values may be raw bytes or use `kb`, `kib`, `mb`, or `mib` suffixes.
/// Resolved once on first access and reused for the lifetime of the process.
pub fn server_function_body_limit() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(
        || match std::env::var("POCOPINE_SERVER_FUNCTION_BODY_LIMIT") {
            Ok(raw) => match parse_body_limit(&raw) {
                Some(limit) => limit,
                None => {
                    tracing::warn!(
                        target: "pocopine.log",
                        value = ?raw,
                        default = DEFAULT_SERVER_FUNCTION_BODY_LIMIT,
                        "invalid POCOPINE_SERVER_FUNCTION_BODY_LIMIT; using default"
                    );
                    DEFAULT_SERVER_FUNCTION_BODY_LIMIT
                }
            },
            Err(_) => DEFAULT_SERVER_FUNCTION_BODY_LIMIT,
        },
    )
}

fn parse_body_limit(raw: &str) -> Option<usize> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }

    let split_at = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at(split_at);
    let number = digits.parse::<usize>().ok()?;
    let multiplier = match unit.trim() {
        "" | "b" => 1,
        "kb" => 1_000,
        "kib" => 1024,
        "mb" => 1_000_000,
        "mib" => 1024 * 1024,
        _ => return None,
    };
    let limit = number.checked_mul(multiplier)?;
    (limit > 0).then_some(limit)
}

/// Serve a directory of static files (typically `index.html` plus the
/// `pkg/` directory that `pocopine build` produces).
///
/// This is a [`tower_http::services::ServeDir`] with pocopine's cache
/// policy applied to every response (fallbacks included):
///
/// * content-hashed bundle files (`<name>.<hash8>.js` /
///   `<name>_bg.<hash8>.wasm`, the pair `pocopine build` writes) →
///   `Cache-Control: public,max-age=31536000,immutable`
/// * HTML responses (index.html, SPA fallbacks) →
///   `Cache-Control: no-cache` (always revalidate, so a fresh deploy's
///   index.html immediately points clients at the new bundle pair)
/// * everything else → no explicit header
///
/// The two together close the wasm/JS cache-skew window: CDN default
/// heuristics cache `.js` but not `.wasm`, which after a deploy can
/// pair a stale cached glue with a fresh wasm and fail
/// `WebAssembly.instantiate` with a `LinkError`. See [`StaticFiles`].
///
/// `/` and `/index.html` serve the GENERATED `pkg/index.html` (the
/// copy `pocopine build` writes with the hashed bundle reference)
/// when it exists, falling back to the source `index.html` — the
/// source keeps the stable unhashed `pkg/<name>.js` reference.
pub fn static_files(dir: impl AsRef<std::path::Path>) -> StaticFiles {
    StaticFiles::new(dir)
}

/// Type-erased handle to an [`AuthProvider`] suitable for use as
/// axum middleware state. Cloning is a `Arc::clone`.
pub type SharedAuthProvider = Arc<dyn AuthProvider>;

/// Axum middleware that runs an [`AuthProvider`] before downstream
/// handlers and inserts the resolved [`Principal`] into the request
/// extensions. Server-function guards (`#[server(guard = ...)]`)
/// then read that principal off `RequestContext`.
///
/// Errors from the provider are logged and treated as anonymous —
/// guards on protected routes will reject the request, public
/// routes still work. Providers that need to **reject** a malformed
/// token at middleware (e.g. JWT verifiers per RFC-070 §5.6) should
/// either return `Ok(None)` for "no credential" and use a different
/// short-circuit mechanism for "bad credential," or layer their own
/// middleware that returns a 401 directly.
async fn auth_middleware(
    State(provider): State<SharedAuthProvider>,
    mut req: Request,
    next: Next,
) -> Response {
    // Build a request context the provider can inspect. Cloning is
    // unfortunate but Method/Uri/HeaderMap/Extensions are all cheap
    // to clone (Extensions clones each entry; standard axum types
    // implement Clone).
    let ctx = RequestContext::from_parts(
        req.method().clone(),
        req.uri().clone(),
        req.headers().clone(),
        req.extensions().clone(),
    );

    match provider.authenticate(&ctx).await {
        Ok(Some(user)) => {
            req.extensions_mut().insert(Principal::from_user(user));
        }
        Ok(None) => {
            // No credential present (or provider explicitly returned
            // anonymous). Don't insert anything — guards see whatever
            // was already there (default = anonymous).
        }
        Err(err) => {
            tracing::warn!(
                target: "pocopine.log",
                error = %err,
                "auth provider failed; treating request as anonymous"
            );
        }
    }

    next.run(req).await
}

/// Extension trait that installs an [`AuthProvider`] on a raw axum
/// [`Router`] in one line.
///
/// Normal applications should prefer [`Server::with_auth`], because
/// [`Server::new`] first installs all linked `#[server]` routes and
/// then the builder method wraps them. This raw-router extension is
/// still useful for tests and custom routers that manually install
/// routes.
///
/// **Install after routes.** `Router::layer` (which this calls under
/// the hood) only wraps the routes that exist at the call site —
/// routes added afterwards silently bypass the middleware. Always call
/// `with_auth` last:
///
/// ```no_run
/// # use pocopine_server::{axum::Router, RouterAuthExt};
/// # use pocopine_auth::{AuthProvider, AuthFuture, AuthUser};
/// # use pocopine_server::RequestContext;
/// # struct StubProvider;
/// # impl AuthProvider for StubProvider {
/// #     fn authenticate<'a>(&'a self, _: &'a RequestContext)
/// #         -> AuthFuture<'a, Option<AuthUser>>
/// #     { Box::pin(async { Ok(None) }) }
/// # }
/// let router = Router::new();
/// // ... manually register routes first ...
/// let router = router.with_auth(StubProvider);
/// ```
///
/// The middleware runs once per request, before any
/// `#[server(guard = ...)]` route handler. Identity flows from
/// middleware → request `Extensions` → `RequestContext` → guard and
/// server-supplied handler extractors.
pub trait RouterAuthExt {
    /// Install an `AuthProvider` as request middleware.
    fn with_auth<P: AuthProvider + 'static>(self, provider: P) -> Self;

    /// Install a pre-`Arc`'d provider — useful when the provider is
    /// shared with other middleware or accessed via app state.
    fn with_auth_arc(self, provider: SharedAuthProvider) -> Self;
}

impl RouterAuthExt for Router {
    fn with_auth<P: AuthProvider + 'static>(self, provider: P) -> Self {
        let provider: SharedAuthProvider = Arc::new(provider);
        self.with_auth_arc(provider)
    }

    fn with_auth_arc(self, provider: SharedAuthProvider) -> Self {
        self.layer(axum::middleware::from_fn_with_state(
            provider,
            auth_middleware,
        ))
    }
}

/// Bind `router` to `addr` and serve forever.
///
/// Compatibility wrapper around [`Server::new(router).serve(addr)`].
/// New code should prefer the builder so plugins can install
/// services, hooks, and tower layers before bind. Do not pre-install
/// generated `__*_route` helpers before calling this wrapper; linked
/// server functions are installed automatically.
pub async fn serve(router: Router, addr: &str) -> std::io::Result<()> {
    Server::new(router).serve(addr).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_body_limit_accepts_bytes_and_units() {
        assert_eq!(parse_body_limit("1024"), Some(1024));
        assert_eq!(parse_body_limit("1mib"), Some(1024 * 1024));
        assert_eq!(parse_body_limit("2mb"), Some(2_000_000));
        assert_eq!(parse_body_limit(" 5kb "), Some(5_000));
    }

    #[test]
    fn parse_body_limit_rejects_zero_and_invalid_values() {
        assert_eq!(parse_body_limit("0"), None);
        assert_eq!(parse_body_limit("0mib"), None);
        assert_eq!(parse_body_limit("banana"), None);
        assert_eq!(parse_body_limit("5gb"), None);
        assert_eq!(parse_body_limit(""), None);
    }
}
