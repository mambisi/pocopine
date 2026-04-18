//! HN server binary — host-only. Serves static files with SPA
//! history-fallback and registers the two `#[server]` routes from
//! `src/lib.rs`.

#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use hn::{__get_item_tree_route, __search_stories_route};
    use pocopine_server::{
        axum::Router,
        serve,
        static_files,
        tower_http::services::ServeFile,
    };

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let index_path = format!("{manifest_dir}/index.html");

    let static_service = static_files(manifest_dir).fallback(ServeFile::new(index_path));
    let router = Router::new().fallback_service(static_service);
    let router = __search_stories_route(router);
    let router = __get_item_tree_route(router);

    let addr = "127.0.0.1:3001";
    println!("▶ serving pocopine HN on http://{addr}");
    serve(router, addr).await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
