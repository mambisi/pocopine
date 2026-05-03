//! Site server binary — host-only.
//!
//! Serves the static site files (index.html + pkg/ + the SPA
//! history-fallback) alongside the `__submit_contact_route`
//! helper emitted by the single `#[server]` function in
//! `src/lib.rs`.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_logging::{init_server_logging, ServerLoggingConfig};
    use pocopine_server::{axum::Router, serve, static_files, tower_http::services::ServeFile};
    use site::__submit_contact_route;

    init_server_logging(ServerLoggingConfig::compact()).map_err(std::io::Error::other)?;

    // Anchor to the crate root so the binary works regardless of CWD.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let index_path = format!("{manifest_dir}/index.html");

    // Static + SPA history fallback.
    let static_service = static_files(manifest_dir).fallback(ServeFile::new(index_path));
    let router = Router::new().fallback_service(static_service);
    let router = __submit_contact_route(router);

    let addr = "127.0.0.1:3000";
    tracing::info!(target: "pocopine.log", %addr, "serving site");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
