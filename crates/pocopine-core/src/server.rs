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
    /// Transport / deserialization failure on the client side.
    Network(String),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::App(msg) => write!(f, "server error: {msg}"),
            ServerError::Network(msg) => write!(f, "network error: {msg}"),
        }
    }
}

impl std::error::Error for ServerError {}

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
