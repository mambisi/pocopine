#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_logging::init_default;
    use pocopine_server::{axum::Router, serve, static_files};

    init_default().map_err(std::io::Error::other)?;

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new().fallback_service(static_files(manifest_dir));

    let addr = "127.0.0.1:3021";
    tracing::info!(target: "pocopine.log", %addr, "serving sync example");
    serve(router, addr).await
}

#[cfg(pocopine_browser)]
fn main() {}
