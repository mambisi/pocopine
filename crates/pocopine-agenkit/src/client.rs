//! Client-side surface (RFC-093 §D9, §D10).
//!
//! What a browser/client touches: the typed flow-call contract and the public
//! stream-event vocabulary. Provider credentials and the runtime never live
//! here — the client calls public flows through generated `#[server]` helpers
//! and consumes redacted [`FlowStreamEvent`]s.
//!
//! This module compiles on every target. Its substance (the typed call helper
//! and the stream consumer) lands in Phase 3; for now it re-exposes the
//! client-safe stream vocabulary from [`crate::shared`].

pub use crate::shared::{FlowStreamEvent, StreamMode};
