//! The wire protocol (RFC 073 §7–§9): the binary frame envelope and the JSON
//! control bodies. **Target-agnostic** — shared by the host [`server`](crate::server)
//! gateway and the wasm [`client`](crate::client), so one codec produces and
//! consumes the same bytes on both ends.
//!
//! - [`Frame`] / [`FrameKind`]: the envelope
//!   (`varUint(kind) varUint(subprotocol_id) varUint(topic_ref) varUint(seq) payload`).
//! - [`Control`] / [`TopicSeq`]: the lifecycle control bodies.
//! - [`ProtocolError`]: decode failures (the gateway maps these into `WsError`).

mod control;
mod error;
mod frame;
mod varint;

pub use control::{Control, TopicSeq};
pub use error::ProtocolError;
pub use frame::{Frame, FrameKind};

/// WebSocket sub-protocol identifier advertised in `Sec-WebSocket-Protocol`
/// and the [`Control::Hello`] frame.
pub const WS_PROTOCOL_V1: &str = "pocopine.ws.v1";

/// Mount path for the gateway upgrade endpoint (the URL a client connects to).
pub const WS_STREAM_PATH: &str = "/__pocopine/ws/v1";

/// Query parameter used by browser clients to carry a bearer token on the
/// WebSocket upgrade. Browsers cannot set arbitrary upgrade headers, so
/// `routes_with_auth` accepts this parameter and authenticates it through the
/// configured `AuthProvider`.
pub const WS_ACCESS_TOKEN_QUERY_PARAM: &str = "access_token";
