//! Server integration (RFC-093 §D15 DC-5): the principal-scoping middleware and
//! the Agenkit server plugin.
//!
//! Flows are internal logic, not endpoints — an app exposes them through plain
//! `#[server]` functions that run a flow by id:
//!
//! ```ignore
//! #[server(public)]
//! pub async fn summarize(input: SummarizeInput) -> ServerResult<Summary> {
//!     active_plugin::<Agenkit>().expect("agenkit_server_plugin installed")
//!         .flow(Summarize).input(input).run().await   // typed marker from #[ai_flow]
//!         .map_err(|e| to_server_error(&e))
//! }
//! ```
//!
//! The wrinkle DC-5 solves: `#[server]` hands the request `Principal` only to
//! guards, not handler bodies. [`PrincipalLayer`] reads the `Principal` that
//! auth put in request extensions and scopes the runtime task-local for the
//! whole request, so the flow above runs under the caller's identity — its
//! tools, retrieval, and threads are principal-scoped (§D5/§D10). The flow body
//! never sees the principal directly; it rides the task-local
//! [`super::agenkit::with_principal`] sets.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::extract::Request;
use axum::response::Response;
use pocopine_auth::Principal;
use pocopine_server::{Server, ServerPlugin};
use tower::{Layer, Service};

use super::agenkit::{Agenkit, with_principal};

/// A tower layer that scopes the Agenkit caller-principal task-local for each
/// request, reading the [`Principal`] from request extensions (anonymous when
/// absent). `Server::with_auth(..)` is applied during server finalization, so it
/// runs before this layer when both are installed on a [`Server`]. For raw axum
/// routers, install the auth middleware so it runs before this layer — or get
/// this layer for free from [`agenkit_server_plugin`].
#[derive(Clone, Copy, Default, Debug)]
pub struct PrincipalLayer;

/// Build a [`PrincipalLayer`] for `Server::layer(..)`.
pub fn principal_layer() -> PrincipalLayer {
    PrincipalLayer
}

impl<S> Layer<S> for PrincipalLayer {
    type Service = PrincipalService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        PrincipalService { inner }
    }
}

/// The service [`PrincipalLayer`] wraps around the inner stack.
#[derive(Clone)]
pub struct PrincipalService<S> {
    inner: S,
}

impl<S> Service<Request> for PrincipalService<S>
where
    S: Service<Request, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request) -> Self::Future {
        let principal = req
            .extensions()
            .get::<Principal>()
            .cloned()
            .unwrap_or_else(Principal::anonymous);
        // tower contract: call the clone that `poll_ready` accepted, not the
        // one we leave behind for the next request.
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(with_principal(
            principal,
            async move { inner.call(req).await },
        ))
    }
}

/// The Agenkit server plugin (mirrors `pocopine_sync` / `pocopine_storage`):
/// registers the runtime handle — reachable from a `#[server]` fn via
/// `pocopine_server::active_plugin::<Agenkit>()` — and installs
/// [`principal_layer`], so flows run under the caller principal.
///
/// ```ignore
/// Server::new(router)
///     .with_auth(my_auth)
///     .plugin(pocopine_agenkit::server::agenkit_server_plugin(agenkit))
///     .serve(addr).await?;
/// ```
pub fn agenkit_server_plugin(agenkit: Agenkit) -> impl ServerPlugin {
    move |server: Server| server.provide_plugin(agenkit).layer(principal_layer())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{AiFlowContext, Flow, MockProvider};
    use axum::body::Body;
    use axum::extract::State;
    use axum::http::Request as HttpRequest;
    use axum::routing::post;
    use http_body_util::BodyExt;
    use pocopine_agenkit_core::{AgenkitResult, ModelRef};
    use pocopine_auth::AuthUser;
    use tower::ServiceExt;

    async fn whoami(_input: (), ctx: AiFlowContext) -> AgenkitResult<String> {
        Ok(ctx
            .principal()
            .user()
            .map(|u| u.id.clone())
            .unwrap_or_else(|| "anon".to_string()))
    }

    fn runtime() -> Agenkit {
        Agenkit::builder()
            .provider(MockProvider::new("local"))
            .default_model(ModelRef::new("local/default"))
            .flow(Flow::new("whoami", whoami)) // internal flow — NOT public
            .build()
            .unwrap()
    }

    // A plain handler that runs a flow by id, exactly like a `#[server]` fn.
    async fn run_whoami(State(agenkit): State<Agenkit>) -> String {
        agenkit.flow("whoami").run::<String>().await.unwrap()
    }

    async fn whoami_through_layer(principal: Option<Principal>) -> String {
        let app = axum::Router::new()
            .route("/run", post(run_whoami))
            .with_state(runtime())
            .layer(principal_layer());
        let mut req = HttpRequest::builder()
            .method("POST")
            .uri("/run")
            .body(Body::empty())
            .unwrap();
        if let Some(principal) = principal {
            req.extensions_mut().insert(principal);
        }
        let response = app.oneshot(req).await.unwrap();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn layer_threads_request_principal_into_a_flow() {
        // DC-5: the principal in extensions reaches `run_flow` via the
        // task-local the layer scopes — without the flow body touching it.
        let who = whoami_through_layer(Some(Principal::from_user(AuthUser::new("alice")))).await;
        assert_eq!(who, "alice");
    }

    #[tokio::test]
    async fn anonymous_without_a_principal_extension() {
        let who = whoami_through_layer(None).await;
        assert_eq!(who, "anon");
    }

    #[test]
    fn server_plugin_installs() {
        // The plugin runs its install (provide_plugin + layer) on a builder.
        let _server = pocopine_server::Server::new(axum::Router::new())
            .plugin(agenkit_server_plugin(runtime()));
    }
}
