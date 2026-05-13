#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use keep_example::{__reset_keep_notes_route, live_backend, sync_server};
    use pocopine_live::{routes, LiveHub};
    use pocopine_logging::init_default;
    use pocopine_server::{axum, axum::Router, serve, static_files, Server};
    use pocopine_sync::sync_server_plugin;

    init_default().map_err(std::io::Error::other)?;

    let sync = sync_server();
    let sync_topics = sync.live_topics().map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(live_backend())
        .allow_topics(sync_topics.clone())
        .default_topics(sync_topics);

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(manifest_dir))
        .layer(axum::middleware::map_response(
            cross_origin_isolation_headers,
        ));
    let router = Server::new(router)
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .map_err(std::io::Error::other)?;
    let router = __reset_keep_notes_route(router);

    let addr = "127.0.0.1:3022";
    tracing::info!(target: "pocopine.log", %addr, "serving keep example");
    serve(router, addr).await
}

#[cfg(pocopine_browser)]
fn main() {}

#[cfg(pocopine_host)]
async fn cross_origin_isolation_headers(
    mut response: pocopine_server::axum::response::Response,
) -> pocopine_server::axum::response::Response {
    use pocopine_server::axum::http::header::{HeaderName, HeaderValue};

    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-embedder-policy"),
        HeaderValue::from_static("require-corp"),
    );
    headers.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    response
}
