//! Transport bridge — the heart of the native target (RFC-104 §4).
//!
//! A pocopine app's `#[server]` functions run host-side behind an axum
//! [`Router`]. On the web that router is bound to a TCP listener and the
//! browser reaches it over HTTP. In a Tauri window there is no network:
//! the webview makes requests under a registered custom URI scheme, and
//! Tauri hands the Rust side an [`http::Request`] for each one. This
//! module turns that request into an axum request, drives the **same**
//! router via [`tower::ServiceExt::oneshot`], and turns the response
//! back into an [`http::Response`] — in-process, no socket, no IPC.
//!
//! Everything here is plain `axum` / `tower` / `http`. It carries **no**
//! dependency on `tauri`, so it compiles and is unit-tested on any host
//! without the webview system libraries. The thin Tauri wiring that
//! calls [`dispatch`] lives in `shell.rs` behind the `tauri` feature.

use std::path::Path;

use http::{Request, Response};
use pocopine_server::axum::body::{to_bytes, Body};
use pocopine_server::axum::Router;
use pocopine_server::tower::ServiceExt;
use pocopine_server::tower_http::services::ServeFile;
use pocopine_server::{index_file, static_files, Server};

/// Assemble the app router that backs the native window.
///
/// Composition mirrors the web server exactly:
///
/// 1. `static_files(root)` with an `index.html` SPA history fallback —
///    serves `index.html`, the wasm `pkg/` bundle, and CSS (with
///    pocopine's cache policy and the generated `pkg/index.html`
///    preference), and answers unmatched non-asset paths with the entry
///    document so client-routed deep links resolve, same as production.
/// 2. [`Server::new`] installs every linked `#[server]` route from the
///    inventory and the RFC-100 asset proxy.
/// 3. `configure` is the app's hook to add auth / plugins / extra routes
///    (`|s| s.with_auth(provider)`); identity by default.
/// 4. [`Server::try_finalize`] validates + activates the plugin registry
///    and returns the finalized router. Native is a single-process,
///    single-server consumer, which is exactly what that path expects.
///
/// `static_root` must contain the app's `index.html` and `pkg/`. In
/// `pocopine native dev` that is the on-disk project directory; in a
/// bundled app it is the Tauri resource directory (see the Tauri shell).
pub fn build_router(
    static_root: impl AsRef<Path>,
    configure: impl FnOnce(Server) -> Server,
) -> std::io::Result<Router> {
    let root = static_root.as_ref();
    let static_service = static_files(root).fallback(ServeFile::new(index_file(root)));
    let router = Router::new().fallback_service(static_service);
    let server = configure(Server::new(router));
    server.try_finalize()
}

/// Drive `router` with one incoming request and collect the full
/// response into memory.
///
/// The body is fully buffered because Tauri's URI-scheme responder
/// answers with an owned byte payload, not a stream — fine for local
/// assets and JSON server-function results. `Router`'s service error is
/// [`std::convert::Infallible`], so this never fails at the routing
/// layer; transport-level failures surface as HTTP status codes from
/// the handlers themselves.
pub async fn dispatch(router: Router, req: Request<Vec<u8>>) -> Response<Vec<u8>> {
    let (parts, body) = req.into_parts();
    let req = Request::from_parts(parts, Body::from(body));

    // `Router<()>: Service<Request<Body>, Error = Infallible>`.
    let response = match router.oneshot(req).await {
        Ok(response) => response,
        Err(never) => match never {},
    };

    let (parts, body) = response.into_parts();
    // Local assets (the wasm bundle is the largest) and JSON results;
    // no artificial cap, matching `static_files`' own test harness.
    let bytes = to_bytes(body, usize::MAX).await.unwrap_or_default();
    Response::from_parts(parts, bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::StatusCode;
    use pocopine_server::axum::routing::get;

    fn get_request(uri: &str) -> Request<Vec<u8>> {
        Request::builder().uri(uri).body(Vec::new()).unwrap()
    }

    #[tokio::test]
    async fn dispatch_routes_to_a_registered_handler() {
        let router = Router::new().route("/_pocopine/ping", get(|| async { "pong" }));
        let res = dispatch(router, get_request("/_pocopine/ping")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.body().as_slice(), b"pong");
    }

    #[tokio::test]
    async fn dispatch_preserves_status_and_headers() {
        let router = Router::new().route(
            "/x",
            get(|| async { (StatusCode::IM_A_TEAPOT, [("x-test", "yes")], "teapot") }),
        );
        let res = dispatch(router, get_request("/x")).await;
        assert_eq!(res.status(), StatusCode::IM_A_TEAPOT);
        assert_eq!(res.headers().get("x-test").unwrap(), "yes");
        assert_eq!(res.body().as_slice(), b"teapot");
    }

    #[tokio::test]
    async fn dispatch_echoes_a_posted_body() {
        // Server-function-shaped: POST a JSON tuple, echo it back.
        let router = Router::new().route(
            "/_pocopine/echo",
            pocopine_server::axum::routing::post(|body: String| async move { body }),
        );
        let req = Request::builder()
            .method("POST")
            .uri("/_pocopine/echo")
            .body(br#"[1,2,3]"#.to_vec())
            .unwrap();
        let res = dispatch(router, req).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.body().as_slice(), br#"[1,2,3]"#);
    }

    #[tokio::test]
    async fn dispatch_returns_404_for_unknown_routes() {
        let router = Router::new().route("/known", get(|| async { "ok" }));
        let res = dispatch(router, get_request("/missing")).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn build_router_serves_static_index_through_the_bridge() {
        // `try_finalize` activates the process-global plugin registry;
        // reset first so a parallel test in this binary can't leave
        // stale state visible here.
        pocopine_server::__reset_for_test();

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            "<!doctype html><h1>native</h1>",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg/app_bg.wasm"), b"\0asm").unwrap();

        let router = build_router(dir.path(), |s| s).unwrap();

        // The document is served from the static fallback.
        let res = dispatch(router.clone(), get_request("/")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.body().as_slice(), b"<!doctype html><h1>native</h1>");

        // The wasm bundle is served from the same router.
        let res = dispatch(router.clone(), get_request("/pkg/app_bg.wasm")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.body().as_slice(), b"\0asm");

        // A client-routed deep link (no matching file) falls back to the
        // entry document so SPA routes resolve on a fresh load.
        let res = dispatch(router, get_request("/connections/42")).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.body().as_slice(), b"<!doctype html><h1>native</h1>");
    }
}
