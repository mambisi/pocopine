//! # pocopine-realtime
//!
//! A generic, payload-agnostic bidirectional WebSocket gateway for pocopine
//! apps (RFC 073, Part I). It is the bidirectional counterpart to
//! `pocopine-live`'s server-to-client SSE invalidation: where SSE is
//! read-only, `pocopine-realtime` lets clients push frames back, which CRDT
//! collaboration (`pocopine-collab`) and messaging (`pocopine-chat`,
//! RFC 106) both require.
//!
//! The gateway owns the transport concerns and nothing else:
//!
//! - a binary [`Frame`] envelope tagged by a `subprotocol_id` it treats as
//!   opaque routing metadata,
//! - Discord-style connection liveness ([`Control::Hello`] /
//!   [`Control::Heartbeat`] / heartbeat-ack / zombie detection),
//!   per-subscription sequence numbers and [`Control::Resume`] replay,
//! - per-topic authorization through the app's [`pocopine_auth::RequestContext`],
//! - fan-out behind the [`Fanout`] trait.
//!
//! Application semantics (CRDT merge, chat ordering/receipts) live in
//! consumer crates, never here.
//!
//! ## Quick start
//!
//! ```no_run
//! # #[cfg(not(target_arch = "wasm32"))]
//! # fn demo() -> axum::Router {
//! use pocopine_realtime::{WsGateway, routes};
//!
//! let gateway = WsGateway::local().allow_all_topics();
//! routes(gateway)
//! # }
//! ```
//!
//! ## Module layout
//!
//! This crate is host-only. Following the `client-server-modules` convention,
//! all server code lives under the [`server`] module, gated once at the crate
//! root; on wasm32 this crate is empty. The server API is also re-exported at
//! the crate root, so `pocopine_realtime::routes` and `pocopine_realtime::server::routes`
//! both resolve.
//!
//! ## Fan-out
//!
//! Fan-out is abstracted by [`Fanout`]. [`LocalFanout`] is the in-process
//! implementation (per-topic ring buffer for sequencing/replay plus a broadcast
//! channel for live delivery). A multi-process implementation backed by the
//! `pocopine-events` Redis spine is the RFC 073 Phase C follow-up; the
//! [`Fanout`] trait is the seam it slots into.

#[cfg(not(target_arch = "wasm32"))]
pub mod server;

#[cfg(not(target_arch = "wasm32"))]
pub use server::*;
