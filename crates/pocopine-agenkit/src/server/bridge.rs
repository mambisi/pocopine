//! Client-safe error boundary for flows (RFC-093 §D9, §D10).
//!
//! Flows are internal logic; an app exposes one through a plain `#[server]`
//! function that runs the flow and maps its error here:
//!
//! ```ignore
//! #[server(public)]
//! pub async fn summarize(input: In) -> ServerResult<Out> {
//!     active_plugin::<Agenkit>().unwrap()
//!         .run_flow("summarize", input).await
//!         .map_err(|e| pocopine_agenkit::server::to_server_error(&e))
//! }
//! ```
//!
//! [`to_server_error`] maps [`AgenkitError`] to `pocopine_core::ServerError`,
//! dropping provider internals so they never cross to the client. The
//! `flow_is_public` / `flow_stream_mode` probes gate the streaming route
//! (`super::stream_route`).

use pocopine_agenkit_core::{AgenkitError, StreamMode};
use pocopine_core::ServerError;

use super::agenkit::Agenkit;

/// Map an [`AgenkitError`] to a client-safe [`ServerError`].
///
/// Validation and tool-policy errors carry their (Agenkit-internal) message;
/// everything that could quote provider internals (provider, config, budget,
/// cancellation, reducer) collapses to the error *kind* only (§D10).
pub fn to_server_error(error: &AgenkitError) -> ServerError {
    match error {
        AgenkitError::Validation { message } => {
            ServerError::bad_request(format!("invalid input: {message}"))
        }
        AgenkitError::ToolPolicy { message } => {
            ServerError::forbidden(format!("tool policy: {message}"))
        }
        AgenkitError::NotFound { .. } => ServerError::bad_request("unknown AI flow"),
        AgenkitError::Json { .. } => ServerError::bad_request("invalid AI payload"),
        // Provider / Config / BudgetExhausted / Cancelled / ReducerDisagreement:
        // never leak internals — surface the stable kind only.
        other => ServerError::App(format!("ai error ({})", other.kind())),
    }
}

impl Agenkit {
    /// Whether a registered flow is public (callable from the client boundary).
    pub fn flow_is_public(&self, id: &str) -> bool {
        self.inner
            .flows
            .get(id)
            .map(|handler| handler.descriptor().public)
            .unwrap_or(false)
    }

    /// The stream-visibility cap the flow author declared via
    /// `Flow::stream_mode` (§D8). Defaults to [`StreamMode::FinalOnly`]: a
    /// flow opts *into* exposing progress events; clients can request less
    /// visibility than the cap, never more.
    pub fn flow_stream_mode(&self, id: &str) -> StreamMode {
        self.inner
            .flows
            .get(id)
            .map(|handler| handler.descriptor().stream_mode)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_errors_do_not_leak_details() {
        let error = AgenkitError::provider("503 from secret-host:8443 with bearer abc");
        let mapped = to_server_error(&error);
        let rendered = mapped.to_string();
        assert!(rendered.contains("ai error (provider)"));
        assert!(!rendered.contains("secret-host"));
        assert!(!rendered.contains("bearer"));
    }

    #[test]
    fn validation_maps_to_bad_request() {
        let mapped = to_server_error(&AgenkitError::validation("missing field `q`"));
        assert!(matches!(mapped, ServerError::BadRequest(_)));
    }

    #[test]
    fn tool_policy_maps_to_forbidden() {
        let mapped = to_server_error(&AgenkitError::tool_policy("not allowlisted"));
        assert!(matches!(mapped, ServerError::Forbidden(_)));
    }
}
