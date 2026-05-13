//! Server-function error types.
//!
//! `#[server]` functions return [`Result<T>`] where the error type is
//! always [`ServerError`]. That keeps the wire protocol uniform and
//! lets the client cleanly distinguish "the server returned an error"
//! from "the network failed or the response didn't deserialize."

use std::fmt;

use serde::{Deserialize, Serialize};

/// All failures a client stub can return.
///
/// * [`ServerError::App`] is serialized by the server as part of a
///   `Result::Err` and decoded verbatim on the client.
/// * [`ServerError::Network`] is synthesized locally when the fetch
///   never reached the server, or the body didn't parse as JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerError {
    /// An application-level error produced on the server side.
    App(String),
    /// Authentication is required but missing or invalid.
    Unauthorized(String),
    /// Authentication succeeded, but this caller cannot perform the action.
    Forbidden(String),
    /// The request payload was malformed or exceeded the framework body limit.
    BadRequest(String),
    /// Transport / deserialization failure on the client side.
    Network(String),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::App(msg) => write!(f, "server error: {msg}"),
            ServerError::Unauthorized(msg) => write!(f, "unauthorized: {msg}"),
            ServerError::Forbidden(msg) => write!(f, "forbidden: {msg}"),
            ServerError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            ServerError::Network(msg) => write!(f, "network error: {msg}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl ServerError {
    /// Build an authentication failure.
    pub fn unauthorized(msg: impl Into<String>) -> Self {
        ServerError::Unauthorized(msg.into())
    }

    /// Build an authorization failure.
    pub fn forbidden(msg: impl Into<String>) -> Self {
        ServerError::Forbidden(msg.into())
    }

    /// Build a malformed request failure.
    pub fn bad_request(msg: impl Into<String>) -> Self {
        ServerError::BadRequest(msg.into())
    }
}

impl From<String> for ServerError {
    fn from(s: String) -> Self {
        ServerError::App(s)
    }
}

impl From<&str> for ServerError {
    fn from(s: &str) -> Self {
        ServerError::App(s.to_owned())
    }
}

/// Canonical `Result` alias for `#[server]` functions.
pub type Result<T> = core::result::Result<T, ServerError>;

const SERVER_FUNCTION_HASH_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const SERVER_FUNCTION_HASH_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = SERVER_FUNCTION_HASH_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(SERVER_FUNCTION_HASH_PRIME);
    }
    hash
}

/// Returns the compiler-generated default route path for a server function.
///
/// The suffix is based on the Rust module path and function name so two
/// modules can expose the same function name without colliding.
#[doc(hidden)]
pub fn server_function_default_path(module_path: &str, function: &str) -> String {
    let mut key = String::with_capacity(module_path.len() + function.len() + 2);
    key.push_str(module_path);
    key.push_str("::");
    key.push_str(function);
    let hash = fnv1a64(key.as_bytes());

    format!("/_pocopine/{function}_{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::server_function_default_path;

    #[test]
    fn default_paths_are_scoped_by_module_path() {
        let first = server_function_default_path("app::filters", "get_filter");
        let second = server_function_default_path("app::admin", "get_filter");

        assert_ne!(first, second);
        assert!(first.starts_with("/_pocopine/get_filter_"));
        assert!(second.starts_with("/_pocopine/get_filter_"));
    }

    #[test]
    fn default_paths_are_stable() {
        assert_eq!(
            server_function_default_path("app::filters", "get_filter"),
            "/_pocopine/get_filter_b0e2ae5de9927498"
        );
    }
}
