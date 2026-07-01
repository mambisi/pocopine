#[cfg(not(target_arch = "wasm32"))]
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    agenkitty::args::run().await
}

#[cfg(target_arch = "wasm32")]
fn main() {}
