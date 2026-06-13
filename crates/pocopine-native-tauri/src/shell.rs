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

    tauri::Builder::default()
        .register_asynchronous_uri_scheme_protocol(SCHEME, move |_app, request, responder| {
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
