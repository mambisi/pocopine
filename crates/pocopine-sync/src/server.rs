use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pocopine_core::{ServerError, ServerResult};
use pocopine_events::SharedEventBackend;
use pocopine_server::axum::extract::State;
use pocopine_server::axum::response::Json;
use pocopine_server::axum::routing::post;
use pocopine_server::{Server, ServerPlugin};
use serde_json::Value;

use crate::{
    sync_shape_tag, SyncError, SyncOpenRequest, SyncOpenResponse, SyncOpenShape, SyncPullRequest,
    SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncResult, SYNC_OPEN_PATH,
    SYNC_PULL_PATH, SYNC_PUSH_PATH,
};

/// Future returned by a shape source.
pub type SyncBoxFuture<'a, T> = Pin<Box<dyn Future<Output = SyncResult<T>> + Send + 'a>>;

/// Server-side source for one registered sync shape.
///
/// Registered shapes are pullable through the sync routes. Until a source
/// implements its own authorization or the host app mounts sync behind an
/// auth boundary, `/open` is validation/discovery and `/pull` remains
/// callable for any registered shape.
pub trait SyncShapeSource: Send + Sync + 'static {
    /// Server-registered shape name.
    fn shape(&self) -> &crate::SyncShapeName;

    /// Public collection name represented by this shape.
    fn collection(&self) -> &crate::SyncCollectionName;

    /// Current cursor, if the source can cheaply report it.
    fn current_cursor(&self) -> Option<crate::SyncCursor> {
        None
    }

    /// Pull changes or a snapshot for this shape.
    fn pull<'a>(&'a self, request: SyncPullRequest) -> SyncBoxFuture<'a, SyncPullResponse<Value>>;

    /// Push client mutations for this shape.
    fn push<'a>(
        &'a self,
        request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'a, SyncPushResponse<Value>> {
        let _ = request;
        Box::pin(async {
            Err(SyncError::unsupported(
                "push is not implemented for this shape",
            ))
        })
    }
}

#[derive(Clone)]
struct SyncServerInner {
    shapes: Arc<HashMap<String, Arc<dyn SyncShapeSource>>>,
    events: Option<SharedEventBackend>,
}

/// Host-side sync server service.
#[derive(Clone)]
pub struct SyncServer {
    inner: Arc<SyncServerInner>,
}

impl SyncServer {
    /// Start building a sync server.
    pub fn builder() -> SyncServerBuilder {
        SyncServerBuilder::default()
    }

    /// Return the live query tags this server can publish for shapes.
    pub fn live_query_tags(&self) -> Vec<String> {
        self.inner
            .shapes
            .keys()
            .map(|shape| sync_shape_tag(shape))
            .collect()
    }

    /// Return the live topics this server can publish for shapes.
    pub fn live_topics(&self) -> pocopine_events::EventResult<Vec<pocopine_events::Topic>> {
        self.live_query_tags()
            .into_iter()
            .map(|tag| pocopine_live::query_tag_topic(&tag))
            .collect()
    }

    /// Publish a live wake-up for one shape when an event backend is
    /// attached. The sync data still moves through pull; this only wakes
    /// browsers to pull with their current sync cursor.
    pub async fn invalidate_shape(&self, shape: &str) -> SyncResult<()> {
        let Some(events) = self.inner.events.as_ref() else {
            return Ok(());
        };
        let tag = sync_shape_tag(shape);
        let topic = pocopine_live::query_tag_topic(&tag)
            .map_err(|err| SyncError::backend(err.to_string()))?;
        let draft = pocopine_live::query_invalidated(topic, [tag])
            .map_err(|err| SyncError::backend(err.to_string()))?;
        events
            .publish(draft)
            .await
            .map_err(|err| SyncError::backend(err.to_string()))?;
        Ok(())
    }

    fn shape(&self, shape: &str) -> SyncResult<Arc<dyn SyncShapeSource>> {
        self.inner
            .shapes
            .get(shape)
            .cloned()
            .ok_or_else(|| SyncError::UnknownShape(shape.to_string()))
    }
}

/// Builder for [`SyncServer`].
#[derive(Default)]
pub struct SyncServerBuilder {
    shapes: HashMap<String, Arc<dyn SyncShapeSource>>,
    events: Option<SharedEventBackend>,
}

impl SyncServerBuilder {
    /// Register one shape source.
    pub fn shape<S>(mut self, shape: S) -> Self
    where
        S: SyncShapeSource,
    {
        self.shapes
            .insert(shape.shape().as_str().to_string(), Arc::new(shape));
        self
    }

    /// Attach the event backend shared with `pocopine-live`.
    pub fn events(mut self, events: SharedEventBackend) -> Self {
        self.events = Some(events);
        self
    }

    /// Finish the sync server.
    pub fn build(self) -> SyncServer {
        SyncServer {
            inner: Arc::new(SyncServerInner {
                shapes: Arc::new(self.shapes),
                events: self.events,
            }),
        }
    }
}

/// Server plugin that mounts sync routes and provides [`SyncServer`].
///
/// Installing this plugin exposes `/__pocopine/sync/v1/open`, `/pull`, and
/// `/push` for every registered shape. Shape sources are responsible for
/// tenant filtering, authorization, and cursor policy in this first slice.
#[derive(Clone)]
pub struct SyncServerPlugin {
    sync: SyncServer,
}

/// Build a sync server plugin.
pub fn sync_server_plugin(sync: SyncServer) -> SyncServerPlugin {
    SyncServerPlugin { sync }
}

impl ServerPlugin for SyncServerPlugin {
    fn name(&self) -> &'static str {
        "pocopine-sync"
    }

    fn install(self, server: Server) -> Server {
        let sync = self.sync;
        server
            .provide_plugin(sync.clone())
            .route(SYNC_OPEN_PATH, post(open_handler).with_state(sync.clone()))
            .route(SYNC_PULL_PATH, post(pull_handler).with_state(sync.clone()))
            .route(SYNC_PUSH_PATH, post(push_handler).with_state(sync))
    }
}

async fn open_handler(
    State(sync): State<SyncServer>,
    Json(request): Json<SyncOpenRequest>,
) -> Json<ServerResult<SyncOpenResponse>> {
    Json(open(sync, request).await.map_err(server_error))
}

async fn open(sync: SyncServer, request: SyncOpenRequest) -> SyncResult<SyncOpenResponse> {
    let mut shapes = Vec::with_capacity(request.shapes.len());
    for requested in request.shapes {
        let shape = sync.shape(requested.as_str())?;
        shapes.push(SyncOpenShape {
            shape: shape.shape().clone(),
            collection: shape.collection().clone(),
            cursor: shape.current_cursor(),
        });
    }
    Ok(SyncOpenResponse::new(shapes))
}

async fn pull_handler(
    State(sync): State<SyncServer>,
    Json(request): Json<SyncPullRequest>,
) -> Json<ServerResult<SyncPullResponse<Value>>> {
    let result = async {
        let shape = sync.shape(request.shape.as_str())?;
        shape.pull(request).await
    }
    .await;
    Json(result.map_err(server_error))
}

async fn push_handler(
    State(sync): State<SyncServer>,
    Json(request): Json<SyncPushRequest<Value>>,
) -> Json<ServerResult<SyncPushResponse<Value>>> {
    let result = async {
        let shape = sync.shape(request.shape.as_str())?;
        shape.push(request).await
    }
    .await;
    Json(result.map_err(server_error))
}

fn server_error(error: SyncError) -> ServerError {
    match error {
        SyncError::InvalidValue { .. } => ServerError::BadRequest(error.to_string()),
        SyncError::UnknownShape(_) => ServerError::Forbidden(error.to_string()),
        SyncError::Unsupported(_) => ServerError::BadRequest(error.to_string()),
        SyncError::Gap(_) => ServerError::BadRequest(error.to_string()),
        SyncError::Json(err) => {
            tracing::error!(target: "pocopine.log", error = %err, "sync json error");
            ServerError::App("sync internal error".to_string())
        }
        SyncError::Client(msg) | SyncError::Backend(msg) => {
            tracing::error!(target: "pocopine.log", error = %msg, "sync backend error");
            ServerError::App("sync internal error".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt;
    use pocopine_core::ServerError;
    use pocopine_server::axum::body::Body;
    use pocopine_server::axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::*;
    use crate::{
        MemorySyncShape, SyncPullMode, SyncPushRequest, SyncRow, SyncShapeName, SYNC_PROTOCOL_V1,
    };

    #[tokio::test]
    async fn pull_route_is_public_for_registered_shapes() {
        let router = router_with_posts_shape();

        let request = SyncPullRequest::new(SyncShapeName::new("posts_for_tenant").unwrap());
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(SYNC_PULL_PATH)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let outer: ServerResult<SyncPullResponse<String>> = serde_json::from_slice(&bytes).unwrap();
        let response = outer.unwrap();
        assert_eq!(response.protocol, SYNC_PROTOCOL_V1);
        assert_eq!(response.mode, SyncPullMode::Snapshot);
        let mut expected = SyncRow::new("post_1", "hello".to_string()).unwrap();
        expected.version = Some(crate::RowVersion::new("v1").unwrap());
        assert_eq!(response.rows, vec![expected]);
    }

    #[tokio::test]
    async fn pull_unknown_shape_returns_forbidden() {
        let router = router_with_posts_shape();

        let request = SyncPullRequest::new(SyncShapeName::new("unknown_shape").unwrap());
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(SYNC_PULL_PATH)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let outer: ServerResult<SyncPullResponse<String>> = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(
            outer,
            Err(ServerError::Forbidden(message)) if message.contains("unknown sync shape")
        ));
    }

    #[tokio::test]
    async fn default_push_returns_unsupported_bad_request() {
        let router = router_with_posts_shape();

        let request = SyncPushRequest::<String> {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            shape: SyncShapeName::new("posts_for_tenant").unwrap(),
            mutations: Vec::new(),
        };
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(SYNC_PUSH_PATH)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let outer: ServerResult<SyncPushResponse<String>> = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(
            outer,
            Err(ServerError::BadRequest(message))
                if message.contains("unsupported sync operation")
        ));
    }

    #[test]
    fn server_error_hides_internal_details() {
        assert!(matches!(
            server_error(SyncError::backend("lock poisoned with internal detail")),
            ServerError::App(message) if message == "sync internal error"
        ));

        let json_err = serde_json::from_str::<serde_json::Value>("not-json").unwrap_err();
        assert!(matches!(
            server_error(SyncError::Json(json_err)),
            ServerError::App(message) if message == "sync internal error"
        ));
    }

    fn router_with_posts_shape() -> pocopine_server::axum::Router {
        pocopine_server::__reset_for_test();
        let posts = MemorySyncShape::<String>::new("posts_for_tenant", "posts").unwrap();
        posts.upsert("post_1", "hello".to_string()).unwrap();
        let sync = SyncServer::builder().shape(posts).build();
        pocopine_server::Server::new(pocopine_server::axum::Router::new())
            .plugin(sync_server_plugin(sync))
            .try_finalize()
            .unwrap()
    }
}
