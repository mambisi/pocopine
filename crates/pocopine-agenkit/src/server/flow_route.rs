//! The public AI-flow route — non-streaming (RFC-093 Phase 3.1, §D9, §D10,
//! §D15 DC-5).
//!
//! A generic `POST` route that runs a public flow once and returns its result.
//! It is the non-streaming sibling of [`super::stream_route`] and the server
//! half of the generated `#[ai_server_flow]` bridge: one route serves every
//! public flow by id, so authors don't hand-write a `#[server]` fn per flow.
//!
//! The DC-5 adapter lives here: the caller [`Principal`] is read from request
//! extensions (populated by `Server::with_auth(...)`) and threaded into the
//! flow, so the flow's tools, retrieval, and thread access run under the real
//! request identity instead of anonymous.

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Extension, Json, Router};
use pocopine_auth::Principal;
use pocopine_core::ServerError;

use super::agenkit::Agenkit;
use super::bridge::to_server_error;

/// The route path; `:id` is the flow id. Mirrors [`super::stream_route`] minus
/// the `/stream` suffix.
pub const AI_FLOW_PATH: &str = "/__pocopine/agenkit/v1/flow/:id";

/// Run a public flow under the caller principal, returning the same
/// `Result<O, ServerError>` envelope the typed client (`fetch::call`) expects.
async fn flow_handler(
    State(agenkit): State<Agenkit>,
    Path(id): Path<String>,
    principal: Option<Extension<Principal>>,
    Json(input): Json<serde_json::Value>,
) -> Json<Result<serde_json::Value, ServerError>> {
    // Only public flows are reachable; a private flow is indistinguishable from
    // an unknown one (§D9/§D10) — same boundary as the streaming route.
    if !agenkit.flow_is_public(&id) {
        return Json(Err(ServerError::bad_request("unknown AI flow")));
    }
    // DC-5: thread the request principal into the flow (anonymous fallback).
    let principal = principal
        .map(|ext| ext.0)
        .unwrap_or_else(Principal::anonymous);
    let result = agenkit
        .run_flow_as(principal, &id, input)
        .await
        .map_err(|error| to_server_error(&error));
    Json(result)
}

/// Build the non-streaming AI-flow router. Mount it alongside the app's routes
/// (and `Server::with_auth(...)` so the principal is populated). Pair it with
/// [`super::stream_route::ai_flow_stream_router`] to also serve streaming flows.
pub fn ai_flow_router(agenkit: Agenkit) -> Router {
    Router::new()
        .route(AI_FLOW_PATH, post(flow_handler))
        .with_state(agenkit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{Agenkit, AiFlowContext, Flow, MockProvider};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use pocopine_agenkit_core::{AgenkitResult, ModelRef};
    use pocopine_auth::{AuthUser, Principal};
    use tower::ServiceExt;

    async fn whoami(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
        Ok(ctx
            .principal()
            .user()
            .map(|u| u.id.clone())
            .unwrap_or_else(|| "anon".to_string()))
    }

    async fn secret(_input: (), _ctx: AiFlowContext) -> AgenkitResult<String> {
        Ok("nope".to_string())
    }

    fn runtime() -> Agenkit {
        Agenkit::builder()
            .provider(MockProvider::new("local"))
            .default_model(ModelRef::new("local/default"))
            .flow(Flow::new("whoami", whoami).public())
            .flow(Flow::new("secret", secret)) // NOT public
            .build()
            .unwrap()
    }

    fn post_flow(id: &str, principal: Option<Principal>) -> Request<Body> {
        let mut req = Request::builder()
            .method("POST")
            .uri(format!("/__pocopine/agenkit/v1/flow/{id}"))
            .header("content-type", "application/json")
            .body(Body::from("null"))
            .unwrap();
        if let Some(principal) = principal {
            req.extensions_mut().insert(principal);
        }
        req
    }

    async fn call(
        id: &str,
        principal: Option<Principal>,
    ) -> (StatusCode, Result<serde_json::Value, ServerError>) {
        let response = ai_flow_router(runtime())
            .oneshot(post_flow(id, principal))
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: Result<serde_json::Value, ServerError> = serde_json::from_slice(&bytes).unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn runs_a_public_flow_under_the_request_principal() {
        // DC-5: the principal in request extensions reaches the flow body.
        let (_status, body) =
            call("whoami", Some(Principal::from_user(AuthUser::new("alice")))).await;
        assert_eq!(body.unwrap(), serde_json::json!("alice"));
    }

    #[tokio::test]
    async fn anonymous_when_no_principal_extension() {
        let (_status, body) = call("whoami", None).await;
        assert_eq!(body.unwrap(), serde_json::json!("anon"));
    }

    #[tokio::test]
    async fn private_flow_is_indistinguishable_from_unknown() {
        let (_status, body) = call("secret", None).await;
        let err = body.unwrap_err();
        assert!(
            format!("{err:?}").contains("unknown AI flow"),
            "got {err:?}"
        );
    }
}
