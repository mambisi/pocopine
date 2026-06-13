//! Site server binary — host-only.
//!
//! Serves the static site files (index.html + pkg/ + the SPA
//! history-fallback). Linked `#[server]` routes are installed by
//! `pocopine_server::Server`.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_logging::init_default;
    use pocopine_server::{
        axum::Router, index_file, serve, static_files, tower_http::services::ServeFile,
    };
    use site as _;

    init_default().map_err(std::io::Error::other)?;

    // Anchor to the crate root so the binary works regardless of CWD.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    // Static + SPA history fallback — `index_file` prefers the
    // generated `pkg/index.html` (hashed bundle reference, written by
    // `pocopine build`) over the source index.html.
    let static_service =
        static_files(manifest_dir).fallback(ServeFile::new(index_file(manifest_dir)));
    let router = Router::new().fallback_service(static_service);

    let addr = "127.0.0.1:3000";
    tracing::info!(target: "pocopine.log", %addr, "serving site");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
