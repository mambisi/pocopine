#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use live_example::{POSTS_COLLECTION, POSTS_LIST_QUERY_TAG, live_backend};
    use pocopine::live::{LiveHub, collection_topic, query_tag_topic, routes};
    use pocopine_logging::init_default;
    use pocopine_server::{axum::Router, serve, static_files};

    init_default().map_err(std::io::Error::other)?;

    let posts_topic = collection_topic(POSTS_COLLECTION).map_err(std::io::Error::other)?;
    let posts_list_topic = query_tag_topic(POSTS_LIST_QUERY_TAG).map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(live_backend())
        .allow_topics([posts_topic.clone(), posts_list_topic.clone()])
        .default_topics([posts_topic, posts_list_topic]);

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(manifest_dir));

    let addr = "127.0.0.1:3020";
    tracing::info!(target: "pocopine.log", %addr, "serving live example");
    serve(router, addr).await
}

#[cfg(pocopine_browser)]
fn main() {}
