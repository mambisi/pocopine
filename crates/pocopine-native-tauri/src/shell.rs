//! Tauri webview wiring (RFC-104 §4) — the **only** module that imports
//! `tauri`, gated behind the `tauri` feature.
//!
//! The app is served over an ephemeral `127.0.0.1` loopback HTTP
//! listener: the window loads `http://127.0.0.1:<port>/`, and the same
//! axum router that the web server would use answers the document, the
//! wasm `pkg/` bundle, CSS, and every `#[server]` / storage call. Real
//! HTTP (rather than a custom URI scheme) is what makes binary uploads
//! work — WebKitGTK crashes reading a `Blob` request body over a custom
//! scheme — and it gives the page a real origin, so server-side
//! origin/CSRF guards see proper `Origin`/`Sec-Fetch-Site` headers.
//!
//! In "server" mode the listener serves the document + wasm + CSS locally
//! and forwards `#[server]`/storage routes to a deployed backend.
//!
//! This module links the platform webview backend (`wry`/`tao`, which
//! need `webkit2gtk-4.1` + `libsoup` on Linux), so it only builds with
//! the `tauri` feature on a desktop host.

use std::sync::Arc;

use pocopine_native::{bridge, dev_dir, NativeApp, NativeAppParts};
use pocopine_server::axum::body::{to_bytes, Body};
use pocopine_server::axum::extract::{Request as AxumRequest, State};
use pocopine_server::axum::response::Response as AxumResponse;
use pocopine_server::axum::routing::any;
use pocopine_server::axum::Router;
use pocopine_server::tokio::net::TcpListener;
use pocopine_server::tower_http::services::ServeFile;
use pocopine_server::{index_file, static_files};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Build the window, bind the loopback server, and run the Tauri event
/// loop until the last window closes. Called via the [`crate::run!`] macro.
pub fn run(app: NativeApp, context: tauri::Context<tauri::Wry>) -> tauri::Result<()> {
    apply_linux_webview_workarounds();

    let NativeAppParts {
        title,
        inner_size,
        configure,
    } = app.into_parts();

    // "server" mode when the CLI passed a backend URL; otherwise the
    // router runs the app's `#[server]` functions in-process.
    let backend = backend_target();
    if let Some((base, _)) = backend.as_ref() {
        tracing::info!(target: "pocopine.log", backend = %base, "native server mode");
    }

    tauri::Builder::default()
        .setup(move |app| {
            let static_root = match dev_dir() {
                // `pocopine native dev` — serve the live project dir.
                Some(dir) => dir,
                // Bundled — serve the resources copied in at build time.
                None => app.path().resource_dir()?,
            };

            let router = match backend {
                Some((base, client)) => proxy_router(&static_root, base, client),
                None => bridge::build_router(static_root, configure)?,
            };

            // Bind an ephemeral loopback port synchronously so the window
            // URL is known, then hand the socket to the async server.
            let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
            std_listener.set_nonblocking(true)?;
            let addr = std_listener.local_addr()?;
            let listener = TcpListener::from_std(std_listener)?;
            tauri::async_runtime::spawn(async move {
                if let Err(err) = pocopine_server::axum::serve(listener, router).await {
                    tracing::error!(
                        target: "pocopine.log",
                        %err,
                        "native loopback server stopped"
                    );
                }
            });
            tracing::info!(target: "pocopine.log", %addr, "native loopback server ready");

            let url = format!("http://{addr}/").parse::<tauri::Url>()?;
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title(title)
                .inner_size(inner_size.0, inner_size.1)
                .build()?;
            Ok(())
        })
        .run(context)
}

// ─── server mode: reverse proxy to the deployed backend ─────────────

#[derive(Clone)]
struct ProxyState {
    base: Arc<String>,
    client: reqwest::Client,
}

/// Router for "server" mode: the `#[server]` / storage routes forward to
/// the backend; everything else (document, wasm, CSS) serves locally with
/// an `index.html` SPA fallback.
fn proxy_router(static_root: &std::path::Path, base: String, client: reqwest::Client) -> Router {
    let static_service =
        static_files(static_root).fallback(ServeFile::new(index_file(static_root)));
    let state = ProxyState {
        base: Arc::new(base),
        client,
    };
    Router::new()
        .route("/_pocopine/*path", any(proxy_handler))
        .route("/__pocopine/*path", any(proxy_handler))
        .fallback_service(static_service)
        .with_state(state)
}

async fn proxy_handler(State(state): State<ProxyState>, req: AxumRequest) -> AxumResponse {
    let (parts, body) = req.into_parts();
    let bytes = to_bytes(body, usize::MAX)
        .await
        .unwrap_or_default()
        .to_vec();
    let response = proxy_to_backend(
        &state.client,
        &state.base,
        http::Request::from_parts(parts, bytes),
    )
    .await;
    let (parts, body) = response.into_parts();
    AxumResponse::from_parts(parts, Body::from(body))
}

/// Forward one request to the remote backend and buffer the full
/// response. Host-to-host (not a browser request) so there is no CORS;
/// the desktop app is a first-party client, so we assert same-origin
/// (the remote's origin guards otherwise reject the loopback `Origin`).
async fn proxy_to_backend(
    client: &reqwest::Client,
    base: &str,
    req: http::Request<Vec<u8>>,
) -> http::Response<Vec<u8>> {
    let (parts, body) = req.into_parts();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or_else(|| parts.uri.path());
    let url = format!("{base}{path_and_query}");

    let mut builder = client.request(parts.method, url).body(body);
    for (name, value) in parts.headers.iter() {
        // Drop the loopback host/origin; the backend is a different one.
        if name == http::header::HOST || name == http::header::ORIGIN {
            continue;
        }
        builder = builder.header(name.clone(), value.clone());
    }
    builder = builder.header("sec-fetch-site", "same-origin");

    match builder.send().await {
        Ok(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            let body = response.bytes().await.unwrap_or_default().to_vec();
            let mut out = http::Response::new(body);
            *out.status_mut() = status;
            *out.headers_mut() = headers;
            out
        }
        Err(err) => {
            tracing::error!(target: "pocopine.log", %err, "native backend proxy failed");
            http::Response::builder()
                .status(http::StatusCode::BAD_GATEWAY)
                .body(b"native backend unreachable".to_vec())
                .expect("static 502 response is well-formed")
        }
    }
}

/// Server-mode proxy target from the environment: the backend base URL
/// (trailing slash trimmed) plus a reusable HTTP client. `None` →
/// standalone (in-process).
fn backend_target() -> Option<(String, reqwest::Client)> {
    let base = std::env::var(BACKEND_ENV)
        .ok()
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())?;

    // rustls 0.23 refuses to auto-pick a CryptoProvider when the build
    // graph enables more than one (this workspace pulls in both aws-lc-rs
    // and ring transitively), panicking at first TLS use. Install ring's
    // provider explicitly before reqwest builds its TLS config. Idempotent
    // — a second call (or one from elsewhere) returns Err, which we ignore.
    let _ = rustls::crypto::ring::default_provider().install_default();

    Some((base, reqwest::Client::new()))
}

/// Environment variable carrying the server backend URL, set by `pocopine
/// native dev|build --backend`. Matches the CLI's `BACKEND_ENV`.
const BACKEND_ENV: &str = "POCOPINE_NATIVE_BACKEND";

/// Disable WebKitGTK's DMABUF renderer, which SIGSEGVs on many Linux
/// setups with NVIDIA / hybrid GPUs under Wayland (WebKitGTK 2.4x+). The
/// fallback compositing path is correct, just slightly slower, so this is
/// safe to default on. Set only when the user hasn't chosen explicitly —
/// `WEBKIT_DISABLE_DMABUF_RENDERER=0` re-enables it. Runs at the top of
/// `run`, before any GTK/WebKit thread starts, so the `set_var` is sound.
#[cfg(target_os = "linux")]
fn apply_linux_webview_workarounds() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }
}

#[cfg(not(target_os = "linux"))]
fn apply_linux_webview_workarounds() {}
