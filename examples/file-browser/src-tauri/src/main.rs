//! Native (Tauri) entry point for the Cloud File Explorer example
//! (RFC-104).
//!
//! Same app, same wasm bundle as the web build — here it's a desktop
//! binary. The storage `#[server]` functions (browse / config /
//! connections) run **in-process** over the custom-protocol bridge, so
//! the native app is a self-contained S3/GCS client with no separate
//! server to deploy.
//!
//! The `configure` hook mirrors `src/bin/server.rs` minus the TCP
//! listener: it installs the same storage plugin so the in-process
//! router serves the storage server functions.

use file_browser_example as _;

fn main() {
    pocopine_native_tauri::run!(pocopine_native_tauri::NativeApp::new()
        .title("Cloud File Explorer")
        .inner_size(1200.0, 800.0)
        .configure(|server| {
            let storage = file_browser_example::storage_server()
                .expect("initialise storage server for the native app");
            server.plugin(pocopine_storage::storage_server_plugin(storage))
        }));
}
