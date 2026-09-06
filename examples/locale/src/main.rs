#[cfg(not(target_arch = "wasm32"))]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    locale_demo::server::run().await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
