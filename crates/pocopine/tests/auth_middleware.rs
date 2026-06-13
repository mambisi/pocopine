// End-to-end check that the `pocopine_server::RouterAuthExt`
// middleware feeds the resolved `Principal` into request extensions
// before macro-generated server-function guards run.
//
// Host-only: the middleware lives in `pocopine-server`, which itself
// is `#![cfg(not(target_arch = "wasm32"))]`.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use pocopine::{AuthUser, ServerResult};
use pocopine_auth::{AuthFuture, AuthProvider, RequestContext};
use pocopine_server::RouterAuthExt;
use pocopine_server::axum::{
    Router,
    body::{Body, to_bytes},
    http::{Method, Request},
};
use pocopine_server::tower::ServiceExt;

/// Stub provider:
/// * `Authorization: Bearer good-token` → `Ok(Some(user))`
/// * No bearer header                    → `Ok(None)` (anonymous)
/// * `Authorization: Bearer bad-token`   → `Err(_)` (provider failure)
#[derive(Clone)]
struct StubProvider;

impl AuthProvider for StubProvider {
    fn authenticate<'a>(&'a self, ctx: &'a RequestContext) -> AuthFuture<'a, Option<AuthUser>> {
        Box::pin(async move {
            match ctx.bearer_token() {
                Some("good-token") => Ok(Some(AuthUser::new("user-1").with_email("a@example.com"))),
                Some("bad-token") => Err(pocopine_auth::AuthError::new("invalid bearer token")),
                Some(_) | None => Ok(None),
            }
        })
    }
}

/// Server function under `pocopine::auth::require_login`. Body is
/// `(name: String,)` per the macro-generated tuple-style decode.
#[pocopine::server(guard = pocopine::auth::require_login)]
async fn whoami(echo: String) -> ServerResult<String> {
    Ok(format!("hello, {echo}"))
}

fn build_router() -> Router {
    // axum's `Router::layer` only wraps routes that exist at the
    // call site, so `with_auth` must run AFTER the server-function
    // route helpers have registered their routes. Get this order
    // wrong and the middleware silently no-ops on the unwrapped
    // routes.
    let router = pocopine_server::axum::Router::new();
    let router = __whoami_route(router);
    router.with_auth(StubProvider)
}

async fn body_text(response: pocopine_server::axum::response::Response) -> String {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn good_token_reaches_guarded_handler() {
    let router = build_router();
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(__whoami_path())
                .header("content-type", "application/json")
                .header("authorization", "Bearer good-token")
                .body(Body::from(r#"["world"]"#))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(response.status().is_success());
    let body = body_text(response).await;
    // Macro wraps the result in Json(Result<R, ServerError>) — happy
    // path is the `Ok` variant.
    assert!(body.contains(r#""Ok":"hello, world""#), "got: {body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn missing_token_falls_through_as_anonymous_and_guard_rejects() {
    let router = build_router();
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(__whoami_path())
                .header("content-type", "application/json")
                .body(Body::from(r#"["world"]"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // The route generator returns Json(Err(ServerError::Unauthorized(_)))
    // which serializes to HTTP 200 with an error envelope (per
    // RFC-066's "typed error in JSON, not HTTP status" decision).
    assert!(response.status().is_success());
    let body = body_text(response).await;
    assert!(
        body.contains(r#""Err":{"Unauthorized":"login required"}"#),
        "got: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn provider_error_treated_as_anonymous_and_guard_rejects() {
    let router = build_router();
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(__whoami_path())
                .header("content-type", "application/json")
                .header("authorization", "Bearer bad-token")
                .body(Body::from(r#"["world"]"#))
                .unwrap(),
        )
        .await
        .unwrap();

    // Provider returned Err(_); middleware logs and falls through as
    // anonymous; guard then rejects with Unauthorized.
    assert!(response.status().is_success());
    let body = body_text(response).await;
    assert!(
        body.contains(r#""Err":{"Unauthorized":"login required"}"#),
        "got: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn with_auth_arc_shares_a_single_provider_handle() {
    // Asserting the Arc-shaped install path compiles and routes
    // identically. Useful when the same provider is referenced by
    // other middleware or by app state.
    let provider: pocopine_server::SharedAuthProvider = Arc::new(StubProvider);
    let router = pocopine_server::axum::Router::new();
    let router = __whoami_route(router);
    let router = router.with_auth_arc(provider);
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(__whoami_path())
                .header("content-type", "application/json")
                .header("authorization", "Bearer good-token")
                .body(Body::from(r#"["world"]"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let body = body_text(response).await;
    assert!(body.contains(r#""Ok":"hello, world""#), "got: {body}");
}
