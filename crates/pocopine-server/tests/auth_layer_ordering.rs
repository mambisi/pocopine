//! Auth installed through `Server::with_auth` is builder-global: plugin routes
//! added later must still receive the principal-populating middleware.

#![cfg(not(target_arch = "wasm32"))]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use pocopine_auth::{AuthFuture, AuthProvider, AuthUser, Principal};
use pocopine_server::axum::body::Body;
use pocopine_server::axum::extract::Request as AxumRequest;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_server::axum::response::Response;
use pocopine_server::axum::routing::get;
use pocopine_server::axum::{Extension, Router};
use pocopine_server::tower::{Layer, Service, ServiceExt};
use pocopine_server::{RequestContext, Server, ServerPlugin};

struct AlwaysAuth;

impl AuthProvider for AlwaysAuth {
    fn authenticate<'a>(&'a self, _ctx: &'a RequestContext) -> AuthFuture<'a, Option<AuthUser>> {
        Box::pin(async { Ok(Some(AuthUser::new("u1"))) })
    }
}

async fn probe(principal: Option<Extension<Principal>>) -> StatusCode {
    if principal.is_some() {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}

async fn status(router: Router, path: &str) -> StatusCode {
    router
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap()
        .status()
}

struct AddsProbeRoute;

impl ServerPlugin for AddsProbeRoute {
    fn install(self, server: Server) -> Server {
        server.route("/plugin", get(probe))
    }
}

#[derive(Clone, Copy, Debug)]
struct SawPrincipal(bool);

#[derive(Clone, Copy, Debug)]
struct CapturePrincipalLayer;

#[derive(Clone)]
struct CapturePrincipalService<S> {
    inner: S,
}

impl<S> Layer<S> for CapturePrincipalLayer {
    type Service = CapturePrincipalService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        CapturePrincipalService { inner }
    }
}

impl<S> Service<AxumRequest> for CapturePrincipalService<S>
where
    S: Service<AxumRequest, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: AxumRequest) -> Self::Future {
        let saw = req.extensions().get::<Principal>().is_some();
        req.extensions_mut().insert(SawPrincipal(saw));
        let clone = self.inner.clone();
        let mut inner = std::mem::replace(&mut self.inner, clone);
        Box::pin(async move { inner.call(req).await })
    }
}

async fn captured_probe(Extension(SawPrincipal(saw)): Extension<SawPrincipal>) -> StatusCode {
    if saw {
        StatusCode::OK
    } else {
        StatusCode::UNAUTHORIZED
    }
}

#[tokio::test]
async fn server_with_auth_wraps_routes_added_afterward() {
    pocopine_server::__reset_for_test();

    let direct = Server::new(Router::new())
        .with_auth(AlwaysAuth)
        .route("/direct", get(probe))
        .try_finalize()
        .unwrap();

    assert_eq!(status(direct, "/direct").await, StatusCode::OK);
}

#[tokio::test]
async fn server_with_auth_wraps_later_plugin_routes() {
    pocopine_server::__reset_for_test();

    let plugin = Server::new(Router::new())
        .with_auth(AlwaysAuth)
        .plugin(AddsProbeRoute)
        .try_finalize()
        .unwrap();

    assert_eq!(status(plugin, "/plugin").await, StatusCode::OK);
}

#[tokio::test]
async fn server_with_auth_runs_before_deferred_layers_that_read_principal() {
    pocopine_server::__reset_for_test();

    let layered = Server::new(Router::new().route("/layered", get(captured_probe)))
        .with_auth(AlwaysAuth)
        .layer(CapturePrincipalLayer)
        .try_finalize()
        .unwrap();

    assert_eq!(status(layered, "/layered").await, StatusCode::OK);
}
