//! Site server binary — host-only.
//!
//! Serves the static site files (index.html + pkg/ + the SPA
//! history-fallback) alongside the `__*_route` helpers emitted by
//! the two `#[server]` functions in `src/lib.rs`.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_server::{
        axum::Router,
        serve,
        static_files,
        tower_http::services::ServeFile,
    };
    use site::{__list_articles_page_route, __submit_contact_route};

    // Anchor to the crate root so `cargo run --bin server` and
    // `pocopine dev` both work regardless of CWD.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let index_path = format!("{manifest_dir}/index.html");

    // Static files + SPA history fallback — unknown paths serve
    // index.html so deep-linked routes (e.g. /perf) survive reload.
    let static_service = static_files(manifest_dir).fallback(ServeFile::new(index_path));
    let router = Router::new().fallback_service(static_service);
    let router = __list_articles_page_route(router);
    let router = __submit_contact_route(router);

    let addr = "127.0.0.1:3000";
    println!("▶ serving site on http://{addr}");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
