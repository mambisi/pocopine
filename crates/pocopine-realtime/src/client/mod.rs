//! The browser client for the `pocopine-realtime` gateway.
//!
//! [`ClientSession`] is the target-agnostic session state machine — driven by
//! the wasm WebSocket I/O shell, but host-testable on its own. The shell itself
//! is `wasm32`-only.

use crate::protocol::WS_ACCESS_TOKEN_QUERY_PARAM;

mod session;
pub use session::{ClientSession, ConnectionStatus, SessionEvent, reconnect_delay_ms};

#[cfg(target_arch = "wasm32")]
mod transport;
#[cfg(target_arch = "wasm32")]
pub use transport::RealtimeClient;

/// Return `url` with `access_token=<token>` appended as a query parameter.
///
/// Browser WebSocket constructors cannot set an `Authorization` header during
/// the upgrade. Pair this with `routes_with_auth` on the server, which verifies
/// the query token through the configured `AuthProvider`.
pub fn url_with_access_token(url: &str, token: &str) -> String {
    pocopine_codec::append_query_param(url, WS_ACCESS_TOKEN_QUERY_PARAM, token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_token_url_appends_and_encodes_query_value() {
        assert_eq!(
            url_with_access_token("ws://localhost/__pocopine/ws/v1", "a b/c"),
            "ws://localhost/__pocopine/ws/v1?access_token=a%20b%2Fc"
        );
        assert_eq!(
            url_with_access_token("wss://example/ws?room=1", "tok+en"),
            "wss://example/ws?room=1&access_token=tok%2Ben"
        );
        assert_eq!(
            url_with_access_token("wss://example/ws#frag", "tok"),
            "wss://example/ws?access_token=tok#frag"
        );
    }
}
