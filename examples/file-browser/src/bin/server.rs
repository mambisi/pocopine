#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use file_browser_example as _;
    use pocopine_logging::init_default;
    use pocopine_server::{
        axum::Router, index_file, static_files, tower_http::services::ServeFile, Server,
    };
    use pocopine_storage::storage_server_plugin;

    init_default().map_err(std::io::Error::other)?;

    let static_dir =
        std::env::var("POCOPINE_DIST").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
    // SPA fallback — `index_file` prefers the generated
    // `pkg/index.html` (hashed bundle reference) over the source one.
    let static_service =
        static_files(&static_dir).fallback(ServeFile::new(index_file(&static_dir)));
    let router = Router::new().fallback_service(static_service);
    let storage = file_browser_example::storage_server().map_err(std::io::Error::other)?;

    let addr = std::env::var("PORT")
        .map(|port| format!("0.0.0.0:{port}"))
        .or_else(|_| std::env::var("POCOPINE_STORAGE_BROWSER_ADDR"))
        .unwrap_or_else(|_| "127.0.0.1:3024".to_string());
    tracing::info!(target: "pocopine.log", %addr, "serving Cloud File Explorer example");
    Server::new(router)
        .plugin(storage_server_plugin(storage))
        .serve(addr.as_str())
        .await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
