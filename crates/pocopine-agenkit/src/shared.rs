//! Vocabulary shared by the [`client`](crate::client) and
//! [`server`](crate::server) modules (RFC-093 §D2).
//!
//! These are the stable, client-safe contract types both sides exchange — the
//! public stream events/modes and the error surface. They compile on every
//! target. Larger or churning contract types stay in `pocopine-agenkit-core`;
//! this module is the curated subset the facade itself trades across the
//! client/server boundary.

pub use pocopine_agenkit_core::{
    AgenkitError, AgenkitResult, FlowStreamEvent, RunId, StreamMode, TraceId,
};
