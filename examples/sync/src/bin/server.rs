#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_live::{routes, LiveHub};
    use pocopine_logging::init_default;
    use pocopine_server::{axum::Router, static_files, Server};
    use pocopine_sync::sync_server_plugin;
    use sync_example::{live_backend, sync_server};

    init_default().map_err(std::io::Error::other)?;

    let sync = sync_server();
    let sync_topics = sync.live_topics().map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(live_backend())
        .allow_topics(sync_topics.clone())
        .default_topics(sync_topics);

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(manifest_dir));
    let addr = "127.0.0.1:3021";
    tracing::info!(target: "pocopine.log", %addr, "serving sync example");
    Server::new(router)
        .plugin(sync_server_plugin(sync))
        .serve(addr)
        .await
}

#[cfg(pocopine_browser)]
fn main() {}
