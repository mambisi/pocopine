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
    use pocopine_server::{axum::Router, serve, static_files, tower_http::services::ServeFile};
    use website as _;

    init_default().map_err(std::io::Error::other)?;

    // Static-asset root. In the deploy Dockerfile this is the
    // `POCOPINE_DIST=/var/lib/pocopine/dist` directory the launcher
    // sets. For `cargo run` locally `CARGO_MANIFEST_DIR` is the right
    // fallback — that's where `index.html` and `pkg/` live in source.
    let static_root =
        std::env::var("POCOPINE_DIST").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_owned());
    let index_path = format!("{static_root}/index.html");

    let static_service = static_files(&static_root).fallback(ServeFile::new(index_path));
    let router = Router::new().fallback_service(static_service);

    let port = std::env::var("PORT").unwrap_or_else(|_| "3001".to_owned());
    let addr = format!("0.0.0.0:{port}");
    tracing::info!(target: "pocopine.log", %addr, %static_root, "serving website");
    serve(router, &addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
