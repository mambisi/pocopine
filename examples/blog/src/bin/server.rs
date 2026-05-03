//! Blog server binary — host-only.
//!
//! Wires the static file server (serves `pkg/` + `index.html`) with the
//! `__get_post_route` helper emitted by the `#[server]` macro. The bin
//! target still exists on wasm32 for workspace consistency, but its
//! `main` is a no-op there.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use blog::__get_post_route;
    use pocopine_logging::{init_server_logging, ServerLoggingConfig};
    use pocopine_server::{axum::Router, serve, static_files};

    init_server_logging(ServerLoggingConfig::compact()).map_err(std::io::Error::other)?;

    // Anchor static files to the crate root, so the server works
    // regardless of the shell's CWD (including when spawned by
    // `pocopine dev` from the workspace root).
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new().fallback_service(static_files(manifest_dir));
    let router = __get_post_route(router);

    let addr = "127.0.0.1:3000";
    tracing::info!(target: "pocopine.log", %addr, "serving blog example");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
