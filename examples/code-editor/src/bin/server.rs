#[cfg(pocopine_host)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    use pocopine_server::{axum::Router, serve, static_files};
    let router = Router::new().fallback_service(static_files(env!("CARGO_MANIFEST_DIR")));
    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(3044);
    serve(router, &format!("127.0.0.1:{port}")).await
}

#[cfg(pocopine_browser)]
fn main() {}
