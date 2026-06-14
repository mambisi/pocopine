//! Tauri webview wiring (RFC-104 §5) — the **only** module that imports
//! `tauri`, gated behind the `tauri` feature.
//!
//! It registers a custom URI scheme whose handler drives the app's axum
//! router via [`pocopine_native::bridge::dispatch`], then opens a window
//! pointed at that scheme. Everything the webview requests — the
//! document, the wasm `pkg/` bundle, CSS, and every
//! `window.fetch("/_pocopine/…")` server call — flows through the same
//! in-process router.
//!
//! This module links the platform webview backend (`wry`/`tao`, which
//! need `webkit2gtk-4.1` + `libsoup` on Linux), so it only builds with
//! the `tauri` feature on a desktop host. The transport logic it calls
//! ([`pocopine_native::bridge`]) is webview-free and unit-tested in
//! `pocopine-native`.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use pocopine_native::{bridge, dev_dir, NativeApp, NativeAppParts};
use pocopine_server::axum::Router;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

/// Custom URI scheme the window's document and all its sub-requests are
/// served under. The browser engine treats it as the document origin,
/// so relative `fetch("/_pocopine/…")` calls stay same-origin and reach
/// the handler below.
const SCHEME: &str = "pocopine";

/// Build the window, register the in-process router, and run the Tauri
/// event loop until the last window closes. Called via the
/// [`crate::run!`] macro.
pub fn run(app: NativeApp, context: tauri::Context<tauri::Wry>) -> tauri::Result<()> {
    apply_linux_webview_workarounds();

    let NativeAppParts {
        title,
        inner_size,
        configure,
    } = app.into_parts();

    // The router is built in `setup` (it needs the resolved static root,
    // and `configure` is `FnOnce`), but the scheme handler is registered
    // before `setup` runs. Share it through a cell the handler reads on
    // each request; `setup` populates it before the window — and thus
    // any request — exists.
    let router_cell: Arc<OnceLock<Router>> = Arc::new(OnceLock::new());
    let handler_cell = Arc::clone(&router_cell);

    // RFC-104 "server" channel: when the CLI passes a backend URL, the
    // app's `#[server]` calls are forwarded to that deployed server
    // (host-side — no browser CORS). Static assets always serve locally.
    // Absent → "standalone": everything runs through the in-process router.
    let proxy = backend_target();
    if let Some((base, _)) = proxy.as_ref() {
        tracing::info!(target: "pocopine.log", backend = %base, "native server channel");
    }

    tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol(SCHEME, move |_app, request, responder| {
            // Server channel: forward server-function / storage routes to
            // the remote backend; everything else (the document, wasm,
            // CSS) still serves from the in-process router.
            if let Some((base, client)) = proxy.as_ref() {
                if is_server_route(request.uri().path()) {
                    let base = base.clone();
                    let client = client.clone();
                    tauri::async_runtime::spawn(async move {
                        let response = proxy_to_backend(&client, &base, request).await;
                        let (parts, body) = response.into_parts();
                        responder.respond(http::Response::from_parts(parts, Cow::Owned(body)));
                    });
                    return;
                }
            }

            let Some(router) = handler_cell.get().cloned() else {
                // Should not happen: the window is created after the cell
                // is set. Fail loud rather than hang the webview.
                tracing::error!(
                    target: "pocopine.log",
                    "native router requested before it was ready"
                );
                responder.respond(service_unavailable());
                return;
            };
            tauri::async_runtime::spawn(async move {
                let response = bridge::dispatch(router, request).await;
                let (parts, body) = response.into_parts();
                responder.respond(http::Response::from_parts(parts, Cow::Owned(body)));
            });
        })
        .setup(move |app| {
            let static_root = match dev_dir() {
                // `pocopine native dev` — serve the live project dir.
                Some(dir) => dir,
                // Bundled — serve the resources copied in at build time.
                None => app.path().resource_dir()?,
            };

            let router = bridge::build_router(static_root, configure)
                .map_err(|err| tauri::Error::Anyhow(err.into()))?;
            // `set` only fails if already set; the cell is private to
            // this run, so first-write always wins.
            let _ = router_cell.set(router);

            let url: WebviewUrl = format!("{SCHEME}://localhost/")
                .parse::<tauri::Url>()
                .map(WebviewUrl::CustomProtocol)
                .map_err(|err| tauri::Error::Anyhow(err.into()))?;

            WebviewWindowBuilder::new(app, "main", url)
                .title(title)
                .inner_size(inner_size.0, inner_size.1)
                .build()?;

            tracing::info!(
                target: "pocopine.log",
                scheme = SCHEME,
                "native window ready"
            );
            Ok(())
        })
        .run(context)
}

/// 503 body returned if a request somehow arrives before the router is
/// installed.
fn service_unavailable() -> http::Response<Cow<'static, [u8]>> {
    http::Response::builder()
        .status(http::StatusCode::SERVICE_UNAVAILABLE)
        .body(Cow::Owned(b"native router not ready".to_vec()))
        .expect("static 503 response is well-formed")
}

/// Environment variable carrying the server-channel backend URL, set by
/// `pocopine native dev|build --channel <name>` / `--backend`. Matches
/// the CLI's `BACKEND_ENV` (RFC-104 contract).
const BACKEND_ENV: &str = "POCOPINE_NATIVE_BACKEND";

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

/// Routes carrying app data to the backend: generated `#[server]`
/// functions (`/_pocopine/…`) and the storage/upload protocol
/// (`/__pocopine/…`). Everything else is a static asset served locally.
fn is_server_route(path: &str) -> bool {
    path.starts_with("/_pocopine") || path.starts_with("/__pocopine")
}

/// Forward one request to the remote backend and buffer the full
/// response. Host-to-host (not a browser request) so there is no CORS;
/// the desktop app is a first-party client, so we assert same-origin like
/// the in-process bridge does.
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
        if name == http::header::HOST {
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
