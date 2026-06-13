#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_live::{LiveHub, routes};
    use pocopine_logging::init_default;
    use pocopine_server::{Server, axum::Router, static_files};
    use pocopine_sync::sync_server_plugin;
    use sync_example::{live_backend, sync_server};

    init_default().map_err(std::io::Error::other)?;

    let sync = sync_server();
    // RFC 088 §C: prefix-based allowlist authorizes both the bare
    // `query:sync:stream:{name}` topic AND per-`(stream, params_hash)`
    // variants `query:sync:stream:{name}:<16-hex>`.
    let sync_topic_prefixes = sync.live_topic_prefixes();
    let default_topics = sync.live_topics().map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(live_backend())
        .allow_topic_prefixes(sync_topic_prefixes)
        .default_topics(default_topics);

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
