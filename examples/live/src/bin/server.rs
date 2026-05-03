#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use live_example::{
        __create_post_route, __list_posts_route, __reset_posts_route, live_backend,
    };
    use pocopine::live::{collection_topic, routes, LiveHub};
    use pocopine_logging::init_default;
    use pocopine_server::{axum::Router, serve, static_files};

    init_default().map_err(std::io::Error::other)?;

    let posts_topic = collection_topic("posts").map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(live_backend())
        .allow_topics([posts_topic.clone()])
        .default_topics([posts_topic]);

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(manifest_dir));
    let router = __list_posts_route(router);
    let router = __create_post_route(router);
    let router = __reset_posts_route(router);

    let addr = "127.0.0.1:3020";
    tracing::info!(target: "pocopine.log", %addr, "serving live example");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
