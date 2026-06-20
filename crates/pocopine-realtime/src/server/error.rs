//! Error type for the `pocopine-realtime` gateway.
//!
//! Hand-rolled `Display`/`Error` impls (no `thiserror`) to match the
//! convention in `pocopine-events`, which also rolls its own error enum.

use std::fmt;

use pocopine_events::EventError;

/// Errors surfaced by the gateway, frame codec, and fan-out layer.
#[derive(Debug)]
pub enum WsError {
    /// A frame could not be decoded (bad varuint, unknown kind, malformed
    /// control JSON, truncated buffer).
    Frame(String),
    /// A topic string could not be resolved to a valid [`pocopine_events::Topic`].
    Topic(EventError),
    /// A frame exceeded the gateway's `max_frame_bytes`.
    TooLarge { size: usize, max: usize },
    /// A live subscription fell behind in real time and lost ordering
    /// (`skipped` messages were dropped). Mirrors
    /// [`pocopine_events::EventError::SubscriptionLagged`].
    Lagged(u64),
    /// A resume cursor is older than retention; the client must re-subscribe
    /// fresh. Mirrors `ReplayBatch.gap == true`.
    Gap,
    /// The connection's `RequestContext` was denied access to a topic.
    Forbidden(String),
    /// The peer violated the protocol (e.g. a Data frame for an unknown
    /// `topic_ref`).
    Protocol(String),
    /// A backend / transport failure in a fan-out implementation (e.g. a
    /// Redis I/O or scripting error).
    Backend(String),
    /// The underlying transport or fan-out source closed.
    Closed,
}

impl WsError {
    /// Build a [`WsError::Frame`] from anything stringy.
    pub fn frame(msg: impl Into<String>) -> Self {
        Self::Frame(msg.into())
    }

    /// Build a [`WsError::Protocol`] from anything stringy.
    pub fn protocol(msg: impl Into<String>) -> Self {
        Self::Protocol(msg.into())
    }

    /// Build a [`WsError::Forbidden`] from anything stringy.
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    /// Build a [`WsError::Backend`] from anything stringy.
    pub fn backend(msg: impl Into<String>) -> Self {
        Self::Backend(msg.into())
    }
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frame(msg) => write!(f, "ws frame error: {msg}"),
            Self::Topic(err) => write!(f, "ws topic error: {err}"),
            Self::TooLarge { size, max } => {
                write!(f, "ws frame too large: {size} bytes (max {max})")
            }
            Self::Lagged(skipped) => {
                write!(f, "ws subscription lagged: {skipped} messages skipped")
            }
            Self::Gap => write!(f, "ws resume cursor not replayable (gap)"),
            Self::Forbidden(topic) => write!(f, "ws topic forbidden: {topic}"),
            Self::Protocol(msg) => write!(f, "ws protocol error: {msg}"),
            Self::Backend(msg) => write!(f, "ws backend error: {msg}"),
            Self::Closed => write!(f, "ws transport closed"),
        }
    }
}

impl std::error::Error for WsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Topic(err) => Some(err),
            _ => None,
        }
    }
}

impl From<EventError> for WsError {
    fn from(err: EventError) -> Self {
        match err {
            EventError::SubscriptionLagged { skipped } => Self::Lagged(skipped),
            EventError::Gap { .. } => Self::Gap,
            other => Self::Topic(other),
        }
    }
}

impl From<crate::protocol::ProtocolError> for WsError {
    fn from(err: crate::protocol::ProtocolError) -> Self {
        match err {
            crate::protocol::ProtocolError::Frame(msg) => Self::Frame(msg),
        }
    }
}
