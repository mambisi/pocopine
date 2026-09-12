//! Pocopine's CLI runs on the host; browser workspace builds have no CLI runtime.

#[cfg(not(target_arch = "wasm32"))]
mod host;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> anyhow::Result<()> {
    host::run()
}

#[cfg(target_arch = "wasm32")]
fn main() {}
