#![cfg(not(target_arch = "wasm32"))]
//! pocopine-server — host-side helpers for `#[server]` functions.
//!
//! This crate is **not** compiled for wasm32. It lives on the server
//! side. Users include it in the `[[bin]] server` target of their app
//! and import the crate's axum re-exports plus the static-file helper
//! that serves the wasm pkg produced by `wasm-pack`.
//!
//! ```no_run
//! use pocopine_server::{axum::{self, Router}, serve, static_files};
//!
//! # mod app { pub fn __get_post_route(r: pocopine_server::axum::Router) -> pocopine_server::axum::Router { r } }
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let router = Router::new()
//!         .nest_service("/", static_files("pkg"))
//!         .merge(Router::new());
//!     let router = app::__get_post_route(router);
//!     serve(router, "0.0.0.0:3000").await
//! }
//! ```

pub use axum;
pub use serde_json;
pub use tokio;
pub use tower;
pub use tower_http;

use std::net::SocketAddr;

use axum::Router;
use tower_http::services::ServeDir;

/// Serve a directory of static files (typically the `pkg/` directory
/// that `wasm-pack build --target web` produces, plus `index.html`).
pub fn static_files(dir: impl AsRef<std::path::Path>) -> ServeDir {
    ServeDir::new(dir)
}

/// Bind `router` to `addr` and serve forever.
pub async fn serve(router: Router, addr: &str) -> std::io::Result<()> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await
}
