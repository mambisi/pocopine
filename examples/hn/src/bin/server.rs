//! HN server binary — host-only. Serves static files with SPA
//! history-fallback; linked `#[server]` routes are installed by
//! `pocopine_server::Server`.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use hn as _;
    use pocopine_logging::init_default;
    use pocopine_server::{axum::Router, serve, static_files, tower_http::services::ServeFile};

    init_default().map_err(std::io::Error::other)?;

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let index_path = format!("{manifest_dir}/index.html");

    let static_service = static_files(manifest_dir).fallback(ServeFile::new(index_path));
    let router = Router::new().fallback_service(static_service);

    let addr = "127.0.0.1:3001";
    tracing::info!(target: "pocopine.log", %addr, "serving pocopine HN");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
