//! Public-flow → server-function bridge (RFC-093 Phase 3.1, §D9, §D10).
//!
//! An app exposes a public flow by writing a `#[server]` function that calls
//! [`Agenkit::run_public_flow`]. The macro generates the typed wasm client and
//! the route; this bridge maps [`AgenkitError`] to `pocopine_core::ServerError`
//! at the boundary, dropping provider internals so they never cross to the
//! client. Only flows marked `.public()` are callable here.

use pocopine_agenkit_core::{AgenkitError, StreamMode};
use pocopine_auth::Principal;
use pocopine_core::ServerError;
use serde::Serialize;
use serde::de::DeserializeOwned;

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

    /// Call a public flow from a `#[server]` function.
    ///
    /// Returns a `ServerError` for non-public/unknown flows and maps any
    /// `AgenkitError` to a client-safe `ServerError`. Provider credentials,
    /// raw prompts, and provider error details never cross this boundary.
    pub async fn run_public_flow<I, O>(&self, id: &str, input: I) -> Result<O, ServerError>
    where
        I: Serialize,
        O: DeserializeOwned,
    {
        if !self.flow_is_public(id) {
            // Do not reveal whether a private flow exists.
            return Err(ServerError::bad_request("unknown AI flow"));
        }
        self.run_flow_typed::<I, O>(id, input)
            .await
            .map_err(|error| to_server_error(&error))
    }

    /// Call a public flow under an explicit caller `principal` (§D15 DC-5).
    ///
    /// Tools and retrieval inside the flow run under `principal`. Use this when
    /// the principal is threaded in explicitly rather than via a scope.
    pub async fn run_public_flow_as<I, O>(
        &self,
        principal: Principal,
        id: &str,
        input: I,
    ) -> Result<O, ServerError>
    where
        I: Serialize,
        O: DeserializeOwned,
    {
        if !self.flow_is_public(id) {
            return Err(ServerError::bad_request("unknown AI flow"));
        }
        let input = serde_json::to_value(input)
            .map_err(|err| ServerError::bad_request(format!("invalid input: {err}")))?;
        let output = self
            .run_flow_as(principal, id, input)
            .await
            .map_err(|err| to_server_error(&err))?;
        serde_json::from_value(output)
            .map_err(|err| to_server_error(&AgenkitError::validation(err.to_string())))
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
