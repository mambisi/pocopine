//! Minimal host-side observability example.
//!
//! The server function intentionally logs only derived fields. Request text can
//! contain user data, so the telemetry path should prove shape and timing
//! without copying raw payloads into logs or spans.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use tracing::Instrument as _;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EchoRequest {
    pub message: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EchoResponse {
    pub message_len: usize,
    pub accepted: bool,
}

#[pocopine::server(public)]
pub async fn observe_echo(request: EchoRequest) -> ServerResult<EchoResponse> {
    let message_len = request.message.len();
    let span = tracing::info_span!(
        target: "pocopine.trace",
        "observability_smoke.echo",
        message_len,
    );

    async move {
        tracing::info!(
            target: "pocopine.log",
            message_len,
            "observability smoke request accepted"
        );

        Ok(EchoResponse {
            message_len,
            accepted: true,
        })
    }
    .instrument(span)
    .await
}
