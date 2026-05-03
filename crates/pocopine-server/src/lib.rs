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
//! async fn main() -> std::io::Result<()> {
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
use std::sync::OnceLock;

use axum::Router;
use tower_http::services::ServeDir;

/// Default maximum JSON request body accepted by generated
/// server-function routes.
pub const DEFAULT_SERVER_FUNCTION_BODY_LIMIT: usize = 2 * 1024 * 1024;

/// Auth contracts passed to `#[server(guard = ...)]` functions.
pub mod auth {
    pub use pocopine_auth::*;
}

/// Server-function body limit in bytes.
///
/// Set `POCOPINE_SERVER_FUNCTION_BODY_LIMIT` to override the default.
/// Values may be raw bytes or use `kb`, `kib`, `mb`, or `mib` suffixes.
/// Resolved once on first access and reused for the lifetime of the process.
pub fn server_function_body_limit() -> usize {
    static LIMIT: OnceLock<usize> = OnceLock::new();
    *LIMIT.get_or_init(|| match std::env::var("POCOPINE_SERVER_FUNCTION_BODY_LIMIT") {
        Ok(raw) => match parse_body_limit(&raw) {
            Some(limit) => limit,
            None => {
                eprintln!(
                    "invalid POCOPINE_SERVER_FUNCTION_BODY_LIMIT={raw:?}; using default {DEFAULT_SERVER_FUNCTION_BODY_LIMIT}"
                );
                DEFAULT_SERVER_FUNCTION_BODY_LIMIT
            }
        },
        Err(_) => DEFAULT_SERVER_FUNCTION_BODY_LIMIT,
    })
}

fn parse_body_limit(raw: &str) -> Option<usize> {
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return None;
    }

    let split_at = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at(split_at);
    let number = digits.parse::<usize>().ok()?;
    let multiplier = match unit.trim() {
        "" | "b" => 1,
        "kb" => 1_000,
        "kib" => 1024,
        "mb" => 1_000_000,
        "mib" => 1024 * 1024,
        _ => return None,
    };
    let limit = number.checked_mul(multiplier)?;
    (limit > 0).then_some(limit)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_body_limit_accepts_bytes_and_units() {
        assert_eq!(parse_body_limit("1024"), Some(1024));
        assert_eq!(parse_body_limit("1mib"), Some(1024 * 1024));
        assert_eq!(parse_body_limit("2mb"), Some(2_000_000));
        assert_eq!(parse_body_limit(" 5kb "), Some(5_000));
    }

    #[test]
    fn parse_body_limit_rejects_zero_and_invalid_values() {
        assert_eq!(parse_body_limit("0"), None);
        assert_eq!(parse_body_limit("0mib"), None);
        assert_eq!(parse_body_limit("banana"), None);
        assert_eq!(parse_body_limit("5gb"), None);
        assert_eq!(parse_body_limit(""), None);
    }
}
