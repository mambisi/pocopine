//! Website server binary — host-only.
//!
//! Serves the static site files (index.html + pkg/) with an SPA
//! history fallback so client-side routes resolve when entered
//! directly. Linked `#[server]` routes are installed by
//! `pocopine_server::Server`.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_logging::init_default;
    use pocopine_server::axum::{
        handler::HandlerWithoutStateExt,
        http::{header, StatusCode, Uri},
        response::IntoResponse,
        Router,
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
    let fallback_root = static_root.clone();

    // SPA history fallback — but only for route-looking paths. Asset-
    // looking misses (last segment has a file extension) get a real 404
    // instead of the index.html shell, matching the CLI dev server: a
    // `text/html` body on e.g. a missing `.webm` makes `<video>` fail
    // with an opaque decoder error rather than a visible 404.
    let spa_fallback = move |uri: Uri| {
        let root = fallback_root.clone();
        async move {
            let last = uri.path().rsplit('/').next().unwrap_or("");
            let looks_like_asset = last
                .rsplit_once('.')
                .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty());
            if looks_like_asset {
                return (StatusCode::NOT_FOUND, "not found").into_response();
            }
            // `index_file` prefers the GENERATED `pkg/index.html` (the
            // copy `pocopine build` writes with the hashed bundle
            // reference) over the source index.html; resolved per
            // request so a fresh build is picked up without a restart.
            let index = pocopine_server::index_file(&root);
            match tokio::fs::read(&index).await {
                Ok(body) => {
                    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
                }
                Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
            }
        }
    };

    let static_service = static_files(&static_root).fallback(spa_fallback.into_service());
    let router = Router::new().fallback_service(static_service);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3001".to_owned());
    let addr = format!("0.0.0.0:{port}");
    tracing::info!(target: "pocopine.log", %addr, %static_root, "serving website");
    serve(router, &addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
