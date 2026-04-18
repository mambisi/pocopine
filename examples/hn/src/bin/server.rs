//! HN server binary — host-only.
//!
//! Axum router serving the static site + the three `#[server]`
//! function routes (`top_stories`, `get_item`, `get_comments`).

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use hn::{__get_comments_route, __get_item_route, __top_stories_route};
    use pocopine_server::{
        axum::Router,
        serve,
        static_files,
        tower_http::services::ServeFile,
    };

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let index_path = format!("{manifest_dir}/index.html");

    // Static + SPA history fallback so /item/:id reloads cleanly.
    let static_service = static_files(manifest_dir).fallback(ServeFile::new(index_path));
    let router = Router::new().fallback_service(static_service);
    let router = __top_stories_route(router);
    let router = __get_item_route(router);
    let router = __get_comments_route(router);

    let addr = "127.0.0.1:3001";
    println!("▶ serving pocopine HN on http://{addr}");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
