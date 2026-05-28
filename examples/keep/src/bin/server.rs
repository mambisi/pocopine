#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use keep_example::{live_backend, sync_server};
    use pocopine_live::{routes, LiveHub};
    use pocopine_logging::init_default;
    use pocopine_server::{axum, axum::Router, static_files, Server};
    use pocopine_sync::sync_server_plugin;

    init_default().map_err(std::io::Error::other)?;

    let sync = sync_server();
    // RFC 088 §C: use `allow_topic_prefixes` so per-`(stream, params_hash)`
    // topics are authorized too. `allow_topics` alone would silently
    // deny per-params subscriptions the moment any source overrides
    // `SyncStreamSource::row_to_params`.
    let sync_topic_prefixes = sync.live_topic_prefixes();
    let default_topics = sync.live_topics().map_err(std::io::Error::other)?;
    let live_hub = LiveHub::new(live_backend())
        .allow_topic_prefixes(sync_topic_prefixes)
        .default_topics(default_topics);

    // $POCOPINE_DIST is set by the deploy container (RFC 080 §4.2);
    // unset under `pocopine dev`, so fall back to the source tree.
    let static_dir =
        std::env::var("POCOPINE_DIST").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
    let cross_origin_isolated = cross_origin_isolation_enabled();
    let mut router = Router::new()
        .merge(routes(live_hub))
        .fallback_service(static_files(&static_dir));

    if cross_origin_isolated {
        router = router.layer(axum::middleware::map_response(
            cross_origin_isolation_headers,
        ));
    }

    // Bind: $PORT (host-injected) → $POCOPINE_KEEP_ADDR → local default.
    let addr = std::env::var("PORT")
        .map(|p| format!("0.0.0.0:{p}"))
        .or_else(|_| std::env::var("POCOPINE_KEEP_ADDR"))
        .unwrap_or_else(|_| "127.0.0.1:3022".to_string());
    tracing::info!(
        target: "pocopine.log",
        %addr,
        cross_origin_isolated,
        "serving keep example"
    );
    Server::new(router)
        .plugin(sync_server_plugin(sync))
        .serve(addr.as_str())
        .await
}

#[cfg(pocopine_browser)]
fn main() {}

#[cfg(pocopine_host)]
fn cross_origin_isolation_enabled() -> bool {
    let Ok(value) = std::env::var("POCOPINE_KEEP_CROSS_ORIGIN_ISOLATED") else {
        return false;
    };

    value == "1"
        || value.eq_ignore_ascii_case("true")
        || value.eq_ignore_ascii_case("yes")
        || value.eq_ignore_ascii_case("on")
}

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
