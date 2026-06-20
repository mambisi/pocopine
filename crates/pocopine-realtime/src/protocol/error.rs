//! Slim error for the wire-protocol codec — frame/control decode failures only.
//!
//! The codec is shared by the host gateway and the wasm client, so it must not
//! depend on the gateway's `pocopine-events`-backed [`WsError`](crate::WsError)
//! (which does not build on wasm). The server folds these in via
//! `From<ProtocolError>`.

use std::fmt;

/// A frame envelope or control body could not be decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    /// Bad varuint, unknown frame kind, truncated buffer, or malformed control
    /// JSON.
    Frame(String),
}

impl ProtocolError {
    /// Build a [`ProtocolError::Frame`] from anything stringy.
    pub fn frame(msg: impl Into<String>) -> Self {
        Self::Frame(msg.into())
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(msg) => write!(f, "ws frame error: {msg}"),
        }
    }
}

impl std::error::Error for ProtocolError {}
