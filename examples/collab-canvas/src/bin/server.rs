//! Dev server for the collab-canvas demo.
//!
//! Mounts the `pocopine-realtime` WebSocket gateway with a `pocopine-collab`
//! `CollabSync` handler registered under the collab sub-protocol, and serves the
//! static files (index.html + the wasm `pkg/`). Both browser sessions (the two
//! iframes in index.html) connect here and share one in-process fan-out, so a
//! rect moved in one session converges in the other.

#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_collab::WsGatewayCollabExt;
    use pocopine_realtime::{WsGateway, routes};
    use pocopine_server::{axum::Router, serve, static_files};

    // `with_collab` registers the CollabSync handler on the gateway's OWN
    // fan-out, so the handler and gateway share one instance by construction (a
    // single-process demo, so no durable store — `with_collab_store` adds one).
    let gateway = WsGateway::local().allow_all_topics().with_collab();

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let router = Router::new()
        .merge(routes(gateway))
        .fallback_service(static_files(manifest_dir));

    // `pocopine run/dev` launches this bin with PORT set (overriding the
    // metadata port); fall back to the metadata default for a bare `cargo run`.
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3030);
    let addr = format!("127.0.0.1:{port}");
    println!("collab-canvas: http://{addr}/  (side-by-side: http://{addr}/dual.html)");
    serve(router, &addr).await
}

#[cfg(pocopine_browser)]
fn main() {}
