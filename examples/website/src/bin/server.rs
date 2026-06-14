//! Website server binary — host-only.
//!
//! Serves the static site files (index.html + pkg/) with an SPA history
//! fallback so client-side routes resolve when entered directly. Linked
//! `#[server]` routes are installed by `pocopine_server::Server`.
//!
//! RFC-099 — set `POCOPINE_SSR=1` to ACTIVATE server-side rendering. Every
//! HTML route is then server-rendered (shell + matched route + component
//! `<style>` + state islands) and returned as a complete document; the
//! wasm bundle hydrates it (the client `main()` auto-detects the
//! server-rendered `[pp-app]`). Unset → the static SPA shell, as before.
//!
//! The component registry is thread-local and the renderer is `!Send`, so
//! all rendering is pinned to one dedicated thread; only the resulting
//! HTML `String` crosses back to the async handler.

#[cfg(not(target_arch = "wasm32"))]
use std::sync::mpsc;

/// A render request: the route path to render, and a channel for the
/// rendered document (`None` if the index is missing or the route
/// doesn't render).
#[cfg(not(target_arch = "wasm32"))]
type SsrRequest = (String, tokio::sync::oneshot::Sender<Option<String>>);

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_logging::init_default;
    use pocopine_server::axum::{
        Router,
        handler::HandlerWithoutStateExt,
        http::{StatusCode, Uri},
        response::IntoResponse,
        routing::get,
    };
    use pocopine_server::{serve, static_files};
    use website as _;

    init_default().map_err(std::io::Error::other)?;

    // Static-asset root. In the deploy Dockerfile this is the
    // `POCOPINE_DIST=/var/lib/pocopine/dist` directory the launcher
    // sets. For `cargo run` locally `CARGO_MANIFEST_DIR` is the right
    // fallback — that's where `index.html` and `pkg/` live in source.
    let static_root =
        std::env::var("POCOPINE_DIST").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_owned());

    // RFC-099 — activate SSR via env. When on, spin up the render thread;
    // the handlers below route HTML requests to it.
    let ssr = std::env::var("POCOPINE_SSR")
        .ok()
        .filter(|v| !v.is_empty() && v != "0" && v != "false")
        .map(|_| spawn_ssr_renderer(static_root.clone()));
    if ssr.is_some() {
        tracing::info!(target: "pocopine.log", "RFC-099 SSR mode active (POCOPINE_SSR)");
    }

    // SPA history fallback — but only for route-looking paths. Asset-
    // looking misses (last segment has a file extension) get a real 404
    // instead of the index.html shell, matching the CLI dev server: a
    // `text/html` body on e.g. a missing `.webm` makes `<video>` fail
    // with an opaque decoder error rather than a visible 404. When SSR is
    // active the matched route is server-rendered; otherwise the static
    // shell is served.
    let spa_fallback = {
        let root = static_root.clone();
        let ssr = ssr.clone();
        move |uri: Uri| {
            let root = root.clone();
            let ssr = ssr.clone();
            async move {
                let last = uri.path().rsplit('/').next().unwrap_or("");
                let looks_like_asset = last
                    .rsplit_once('.')
                    .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty());
                if looks_like_asset {
                    return (StatusCode::NOT_FOUND, "not found").into_response();
                }
                serve_html_route(&ssr, &root, uri.path()).await
            }
        }
    };

    let mut router = Router::new();
    // `/` and `/index.html` are otherwise served directly by ServeDir as
    // the static shell, bypassing the fallback — so when SSR is active,
    // route them explicitly to the renderer too.
    if ssr.is_some() {
        let html_root = {
            let ssr = ssr.clone();
            let root = static_root.clone();
            move || {
                let ssr = ssr.clone();
                let root = root.clone();
                async move { serve_html_route(&ssr, &root, "/").await }
            }
        };
        router = router
            .route("/", get(html_root.clone()))
            .route("/index.html", get(html_root));
    }

    let static_service = static_files(&static_root).fallback(spa_fallback.into_service());
    let router = router.fallback_service(static_service);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3001".to_owned());
    let addr = format!("0.0.0.0:{port}");
    tracing::info!(target: "pocopine.log", %addr, %static_root, "serving website");
    serve(router, &addr).await
}

/// Serve one HTML route: the server-rendered document when SSR is active
/// and the route renders, else the static `index.html` shell.
#[cfg(not(target_arch = "wasm32"))]
async fn serve_html_route(
    ssr: &Option<mpsc::Sender<SsrRequest>>,
    root: &str,
    path: &str,
) -> pocopine_server::axum::response::Response {
    use pocopine_server::axum::{
        http::{StatusCode, header},
        response::IntoResponse,
    };

    if let Some(tx) = ssr {
        if let Some(html) = render_via(tx, path).await {
            return ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
        }
        // SSR failed (missing index / unrenderable route) — fall through
        // to the static shell so the client still boots and mounts.
    }
    // `index_file` prefers the GENERATED `pkg/index.html` (the copy
    // `pocopine build` writes with the hashed bundle reference) over the
    // source index.html; resolved per request so a fresh build is picked
    // up without a restart.
    let index = pocopine_server::index_file(root);
    match tokio::fs::read(&index).await {
        Ok(body) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

/// Send a render request to the SSR thread and await the document.
#[cfg(not(target_arch = "wasm32"))]
async fn render_via(tx: &mpsc::Sender<SsrRequest>, path: &str) -> Option<String> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((path.to_owned(), reply_tx)).ok()?;
    reply_rx.await.ok().flatten()
}

/// RFC-099 — spawn the dedicated SSR render thread. It registers the app
/// (the component registry is thread-local) once, snapshots every
/// component's CSS, then renders each requested path to a full HTML
/// document. The thread lives for the process; it exits when all senders
/// drop (server shutdown).
#[cfg(not(target_arch = "wasm32"))]
fn spawn_ssr_renderer(static_root: String) -> mpsc::Sender<SsrRequest> {
    let (tx, rx) = mpsc::channel::<SsrRequest>();
    std::thread::Builder::new()
        .name("pocopine-ssr".to_owned())
        .spawn(move || {
            // Register components + routes in THIS thread's registry, then
            // snapshot the component CSS for the document.
            let _app = website::boot_app();
            let styles = pocopine_ssr::collected_style_tags();
            while let Ok((path, reply)) = rx.recv() {
                let _ = reply.send(render_ssr_document(&static_root, &styles, &path));
            }
        })
        .expect("spawn pocopine-ssr render thread");
    tx
}

/// Render one route to a complete HTML document: the built `index.html`
/// shell (hashed bundle + stylesheet) with its empty `[pp-app]` replaced
/// by the server-rendered shell + route + component styles + state
/// islands. `None` if the index is missing or the route doesn't render.
#[cfg(not(target_arch = "wasm32"))]
fn render_ssr_document(static_root: &str, styles: &str, path: &str) -> Option<String> {
    let index = std::fs::read_to_string(pocopine_server::index_file(static_root)).ok()?;
    let page = pocopine_ssr::render_app_to_string(&website::WebsiteApp::default(), path)?;
    let inner = format!(
        "{styles}<website-app>{}{}</website-app>",
        page.body,
        page.state_island()
    );
    splice_pp_app(&index, &inner)
}

/// Replace the empty `<div pp-app>…</div>` block in `index` with `inner`.
/// The shell's `pp-app` holds only `<website-app></website-app>`, so the
/// first `</div>` after the open tag closes the block.
#[cfg(not(target_arch = "wasm32"))]
fn splice_pp_app(index: &str, inner: &str) -> Option<String> {
    const OPEN: &str = "<div pp-app>";
    let start = index.find(OPEN)?;
    let after = start + OPEN.len();
    let close = after + index[after..].find("</div>")?;
    let mut out = String::with_capacity(index.len() + inner.len());
    out.push_str(&index[..after]);
    out.push_str(inner);
    out.push_str(&index[close..]);
    Some(out)
}

#[cfg(target_arch = "wasm32")]
fn main() {}
