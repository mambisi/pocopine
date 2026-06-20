//! The browser client for the `pocopine-realtime` gateway.
//!
//! [`ClientSession`] is the target-agnostic session state machine — driven by
//! the wasm WebSocket I/O shell, but host-testable on its own. The shell itself
//! is `wasm32`-only.

mod session;
pub use session::{ClientSession, SessionEvent};

#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(target_arch = "wasm32")]
pub use transport::RealtimeClient;
