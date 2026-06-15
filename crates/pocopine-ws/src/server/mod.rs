//! Host-side gateway implementation (RFC 073 Part I).
//!
//! Everything host-only lives under this module so the crate root needs only
//! a single `#[cfg(not(target_arch = "wasm32"))]` gate rather than one per
//! item (see the `client-server-modules` skill). Submodules below carry no
//! `cfg` attributes themselves.

mod control;
mod error;
mod fanout;
mod frame;
mod gateway;
mod route;
mod varint;

pub use control::{Control, TopicSeq};
pub use error::WsError;
pub use fanout::{DEFAULT_TOPIC_CAPACITY, Fanout, LocalFanout, TopicMessage, TopicStream};
pub use frame::{Frame, FrameKind};
pub use gateway::{GatewayConfig, TopicPolicy, TopicResolver, WS_PROTOCOL_V1, WsGateway};
pub use route::{WS_STREAM_PATH, routes};
