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
