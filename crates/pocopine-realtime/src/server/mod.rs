//! Host-side gateway implementation (RFC 073 Part I).
//!
//! Everything host-only lives under this module so the crate root needs only
//! a single `#[cfg(not(target_arch = "wasm32"))]` gate rather than one per
//! item (see the `client-server-modules` skill). Submodules below carry no
//! `cfg` attributes themselves. The wire protocol (frame envelope + control
//! bodies) is the shared [`crate::protocol`] module.

mod error;
mod fanout;
mod gateway;
mod handler;
#[cfg(feature = "redis")]
mod redis_fanout;
mod route;

pub use error::WsError;
pub use fanout::{
    DEFAULT_TOPIC_CAPACITY, Fanout, LocalFanout, TopicMessage, TopicSource, TopicStream,
};
pub use gateway::{GatewayConfig, TopicPolicy, TopicResolver, WsGateway};
pub use handler::{InboundData, Reaction, SubprotocolHandler};
#[cfg(feature = "redis")]
pub use redis_fanout::{DEFAULT_REDIS_MAX_LEN, RedisFanout};
pub use route::routes;
