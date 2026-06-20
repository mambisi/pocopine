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
    use std::sync::Arc;

    use pocopine_collab::{COLLAB_SUBPROTOCOL, CollabSync};
    use pocopine_realtime::{Fanout, LocalFanout, WsGateway, routes};
    use pocopine_server::{axum::Router, serve, static_files};

    // One shared fan-out: the gateway and the CollabSync handler must publish
    // and subscribe through the same instance (the convergence apply loop).
    let fanout: Arc<dyn Fanout> = Arc::new(LocalFanout::new());
    let gateway = WsGateway::new(fanout.clone())
        .allow_all_topics()
        .with_handler(COLLAB_SUBPROTOCOL, Arc::new(CollabSync::new(fanout)));

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
