use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use pocopine_core::{ServerError, ServerResult};
use pocopine_events::SharedEventBackend;
use pocopine_server::RequestContext;
use pocopine_server::auth::{
    AuthUser, Predicate, Principal, RequestAuthExt, Role, require_auth, require_role,
};
use pocopine_server::axum::Router;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::*;
use crate::{
    ClientMutation, MemorySyncStream, MutationId, RowKey, SYNC_OPEN_PATH, SYNC_PROTOCOL_V1,
    SYNC_PULL_PATH, SYNC_PUSH_PATH, SyncCollectionName, SyncError, SyncOp, SyncOpenRequest,
    SyncOpenResponse, SyncPullMode, SyncPullRequest, SyncPullResponse, SyncPushRequest,
    SyncPushResponse, SyncResult, SyncRow, SyncStreamName, sync_stream_tag,
};

#[tokio::test]
async fn pull_route_is_public_for_registered_streams() {
    let router = router_with_posts_stream();

    let request = SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap());
    let outer: ServerResult<SyncPullResponse<String>> =
        post_json(router, SYNC_PULL_PATH, &request, None).await;
    let response = outer.unwrap();
    assert_eq!(response.protocol, SYNC_PROTOCOL_V1);
    assert_eq!(response.mode, SyncPullMode::Snapshot);
    let mut expected = SyncRow::new("post_1", "hello".to_string()).unwrap();
    expected.version = Some(crate::RowVersion::new("v1").unwrap());
    assert_eq!(response.rows, vec![expected]);
}

#[tokio::test]
async fn pull_unknown_stream_returns_forbidden() {
    let router = router_with_posts_stream();

    let request = SyncPullRequest::new(SyncStreamName::new("unknown_stream").unwrap());
    let outer: ServerResult<SyncPullResponse<String>> =
        post_json(router, SYNC_PULL_PATH, &request, None).await;
    assert!(matches!(
        outer,
        Err(ServerError::Forbidden(message)) if message.contains("unknown sync stream")
    ));
}

#[tokio::test]
async fn push_route_applies_memory_mutation_for_registered_streams() {
    let router = router_with_posts_stream();

    let request = SyncPushRequest::new(
        SyncStreamName::new("posts_for_tenant").unwrap(),
        [ClientMutation {
            id: MutationId::new("device_1:1").unwrap(),
            key: Some(RowKey::new("post_2").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: "from push".to_string(),
            migration_outcome: None,
        }],
    );
    let outer: ServerResult<SyncPushResponse<String>> =
        post_json(router, SYNC_PUSH_PATH, &request, None).await;
    let response = outer.unwrap();
    assert_eq!(response.accepted.len(), 1);
    assert!(response.rejected.is_empty());
    assert!(response.conflicts.is_empty());
    assert_eq!(response.rows[0].key.as_str(), "post_2");
    assert_eq!(response.rows[0].value, "from push");
}

#[tokio::test]
async fn guarded_stream_denies_anonymous_open_and_pull() {
    let router = router_with_guarded_posts_stream(require_auth());

    let open = SyncOpenRequest::new([SyncStreamName::new("posts_for_tenant").unwrap()]);
    let opened: ServerResult<SyncOpenResponse> =
        post_json(router.clone(), SYNC_OPEN_PATH, &open, None).await;
    assert!(matches!(opened, Err(ServerError::Unauthorized(_))));

    let pull = SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap());
    let pulled: ServerResult<SyncPullResponse<String>> =
        post_json(router, SYNC_PULL_PATH, &pull, None).await;
    assert!(matches!(pulled, Err(ServerError::Unauthorized(_))));
}

#[tokio::test]
async fn guarded_stream_allows_authenticated_principal() {
    let router = router_with_guarded_posts_stream(require_auth());
    let principal = Principal::from_user(AuthUser::new("user_1"));

    let pull = SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap());
    let pulled: ServerResult<SyncPullResponse<String>> =
        post_json(router, SYNC_PULL_PATH, &pull, Some(principal)).await;

    assert!(pulled.is_ok());
}

#[tokio::test]
async fn role_guard_checks_authenticated_principal_roles() {
    let router = router_with_guarded_posts_stream(require_role("admin"));
    let viewer = Principal::from_user(AuthUser::new("viewer").with_role(Role::user()));
    let admin = Principal::from_user(AuthUser::new("admin").with_role(Role::admin()));
    let pull = SyncPullRequest::new(SyncStreamName::new("posts_for_tenant").unwrap());

    let denied: ServerResult<SyncPullResponse<String>> =
        post_json(router.clone(), SYNC_PULL_PATH, &pull, Some(viewer)).await;
    assert!(matches!(denied, Err(ServerError::Forbidden(_))));

    let allowed: ServerResult<SyncPullResponse<String>> =
        post_json(router, SYNC_PULL_PATH, &pull, Some(admin)).await;
    assert!(allowed.is_ok());
}

#[tokio::test]
async fn source_receives_request_context_after_guard_passes() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sync = SyncServer::builder()
        .public_stream(ContextCaptureStream::new(seen.clone()))
        .build();
    let router = finalize(sync);
    let principal = Principal::from_user(AuthUser::new("ctx_user"));
    let pull = SyncPullRequest::new(SyncStreamName::new("context_stream").unwrap());

    let pulled: ServerResult<SyncPullResponse<String>> =
        post_json(router, SYNC_PULL_PATH, &pull, Some(principal)).await;

    assert!(pulled.is_ok());
    assert_eq!(seen.lock().unwrap().as_slice(), &["ctx_user".to_string()]);
}

#[tokio::test]
async fn guarded_stream_denies_push_before_default_unsupported() {
    let router = router_with_guarded_posts_stream(require_auth());
    let request = SyncPushRequest::<String> {
        protocol: SYNC_PROTOCOL_V1.to_string(),
        stream: SyncStreamName::new("posts_for_tenant").unwrap(),
        params: ::std::collections::BTreeMap::new(),
        schema_version: crate::default_schema_version_one(),
        mutations: Vec::new(),
    };

    let pushed: ServerResult<SyncPushResponse<String>> =
        post_json(router, SYNC_PUSH_PATH, &request, None).await;

    assert!(matches!(pushed, Err(ServerError::Unauthorized(_))));
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

#[test]
fn schema_migration_error_maps_to_bad_request() {
    let err = server_error(SyncError::schema_migration("posts", 1, 2));
    match err {
        ServerError::BadRequest(msg) => {
            assert!(msg.contains("posts"), "unexpected message: {msg}");
            assert!(msg.contains("v1"), "missing from-version: {msg}");
            assert!(msg.contains("v2"), "missing to-version: {msg}");
        }
        other => panic!("expected BadRequest, got: {other:?}"),
    }
}

#[tokio::test]
async fn open_response_includes_schema_version_from_source() {
    // Default MemorySyncStream returns schema_version = 1 via the
    // trait default. The wire response should carry that explicitly.
    let router = router_with_posts_stream();
    let open = SyncOpenRequest::new([SyncStreamName::new("posts_for_tenant").unwrap()]);
    let opened: SyncOpenResponse = post_json(router, SYNC_OPEN_PATH, &open, None)
        .await
        .unwrap();
    assert_eq!(opened.streams.len(), 1);
    assert_eq!(opened.streams[0].schema_version, 1);
}

#[test]
fn open_response_without_schema_version_field_deserializes_as_one() {
    // Backwards-compat: an old server response that omits the field
    // must round-trip through serde with schema_version = 1. Use a
    // literal JSON string (not the `json!{}` macro) so the test
    // exercises the actual wire-deserialization path.
    let payload = format!(
        r#"{{"protocol":"{SYNC_PROTOCOL_V1}","streams":[{{"stream":"posts_for_tenant","collection":"posts","cursor":null}}]}}"#
    );
    let response: SyncOpenResponse = serde_json::from_str(&payload).unwrap();
    assert_eq!(response.streams[0].schema_version, 1);
}

fn router_with_posts_stream() -> pocopine_server::axum::Router {
    let posts = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    posts.upsert("post_1", "hello".to_string()).unwrap();
    let sync = SyncServer::builder().public_stream(posts).build();
    finalize(sync)
}

// ---- RFC 088 §C: invalidate_stream_with_row dual publish -------

/// Test source: overrides `row_to_params` to project
/// `workspace_id` into the params map so we can assert the
/// per-params topic is published.
struct ParamsRoutedStream {
    name: SyncStreamName,
    collection: SyncCollectionName,
}
impl ParamsRoutedStream {
    fn new() -> Self {
        Self {
            name: SyncStreamName::new("issues").unwrap(),
            collection: SyncCollectionName::new("issues").unwrap(),
        }
    }
}
impl SyncStreamSource for ParamsRoutedStream {
    fn stream(&self) -> &SyncStreamName {
        &self.name
    }
    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }
    fn pull(
        &self,
        _ctx: RequestContext,
        _request: SyncPullRequest,
    ) -> SyncBoxFuture<'_, SyncPullResponse<Value>> {
        Box::pin(async move { Err(SyncError::Unsupported("test stream".into())) })
    }
    fn push(
        &self,
        _ctx: RequestContext,
        _request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'_, SyncPushResponse<Value>> {
        Box::pin(async move { Err(SyncError::Unsupported("test stream".into())) })
    }
    fn row_to_params(&self, row: &Value) -> SyncResult<crate::StreamParams> {
        let mut params = crate::StreamParams::new();
        if let Some(ws) = row.get("workspace_id") {
            params.insert("workspace_id".into(), ws.clone());
        }
        Ok(params)
    }
}

#[tokio::test]
async fn invalidate_stream_with_row_publishes_per_params_only() {
    // Post-codex-review (P2.2): `invalidate_stream_with_row` no
    // longer emits the bare-topic publish. The caller (the
    // `/push` handler) issues a SINGLE bare publish per push
    // and lets this helper publish only the per-params topic
    // per row.
    use pocopine_events::{MemoryEventBackend, ReplayRequest};
    use std::sync::Arc;

    let backend: SharedEventBackend = Arc::new(MemoryEventBackend::new());
    let sync = SyncServer::builder()
        .public_stream(ParamsRoutedStream::new())
        .events(backend.clone())
        .build();

    let row = json!({"id": "i1", "workspace_id": "W1"});
    sync.invalidate_stream_with_row("issues", &row)
        .await
        .unwrap();

    let bare_tag = sync_stream_tag("issues");
    let mut expected_params = crate::StreamParams::new();
    expected_params.insert("workspace_id".into(), json!("W1"));
    let hash = crate::stream_params_hash("issues", &expected_params);
    let params_tag = crate::sync_stream_params_tag("issues", hash);
    let bare_topic = pocopine_live::query_tag_topic(&bare_tag).unwrap();
    let params_topic = pocopine_live::query_tag_topic(&params_tag).unwrap();

    let replay = backend
        .replay(ReplayRequest::new(vec![
            bare_topic.clone(),
            params_topic.clone(),
        ]))
        .await
        .unwrap();
    let bare_count = replay
        .events
        .iter()
        .filter(|e| e.topic == bare_topic)
        .count();
    let params_count = replay
        .events
        .iter()
        .filter(|e| e.topic == params_topic)
        .count();
    assert_eq!(bare_count, 0, "no bare publish (caller's responsibility)");
    assert_eq!(params_count, 1, "exactly one per-params publish");
}

/// When `row_to_params` returns empty params (the default impl,
/// or a source that opts out), the per-params publish is a
/// no-op — and since the bare publish is now the caller's
/// responsibility, this test verifies the helper emits NO
/// events at all in that case.
struct DefaultRoutedStream {
    name: SyncStreamName,
    collection: SyncCollectionName,
}
impl DefaultRoutedStream {
    fn new() -> Self {
        Self {
            name: SyncStreamName::new("comments").unwrap(),
            collection: SyncCollectionName::new("comments").unwrap(),
        }
    }
}
impl SyncStreamSource for DefaultRoutedStream {
    fn stream(&self) -> &SyncStreamName {
        &self.name
    }
    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }
    fn pull(
        &self,
        _ctx: RequestContext,
        _request: SyncPullRequest,
    ) -> SyncBoxFuture<'_, SyncPullResponse<Value>> {
        Box::pin(async move { Err(SyncError::Unsupported("test stream".into())) })
    }
    fn push(
        &self,
        _ctx: RequestContext,
        _request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'_, SyncPushResponse<Value>> {
        Box::pin(async move { Err(SyncError::Unsupported("test stream".into())) })
    }
    // No override of row_to_params → trait default → empty params.
}

#[tokio::test]
async fn invalidate_stream_with_row_skips_per_params_when_empty() {
    use pocopine_events::{MemoryEventBackend, ReplayRequest};
    use std::sync::Arc;

    let backend: SharedEventBackend = Arc::new(MemoryEventBackend::new());
    let sync = SyncServer::builder()
        .public_stream(DefaultRoutedStream::new())
        .events(backend.clone())
        .build();

    let row = json!({"id": "c1"});
    sync.invalidate_stream_with_row("comments", &row)
        .await
        .unwrap();

    let bare_tag = sync_stream_tag("comments");
    let bare_topic = pocopine_live::query_tag_topic(&bare_tag).unwrap();
    let replay = backend
        .replay(ReplayRequest::new(vec![bare_topic.clone()]))
        .await
        .unwrap();
    // No events: the helper publishes per-params only, and
    // row_to_params returned empty so the per-params publish
    // was skipped. The bare invalidation is the `/push`
    // handler's responsibility (a separate code path), so this
    // direct invocation produces zero events.
    assert_eq!(replay.events.len(), 0);
}

fn router_with_guarded_posts_stream<P>(predicate: P) -> Router
where
    P: Predicate,
{
    let posts = MemorySyncStream::<String>::new("posts_for_tenant", "posts").unwrap();
    posts.upsert("post_1", "hello".to_string()).unwrap();
    let sync = SyncServer::builder()
        .guarded_stream(posts, predicate)
        .build();
    finalize(sync)
}

fn finalize(sync: SyncServer) -> Router {
    pocopine_server::__reset_for_test();
    pocopine_server::Server::new(pocopine_server::axum::Router::new())
        .plugin(sync_server_plugin(sync))
        .try_finalize()
        .unwrap()
}

async fn post_json<T, R>(
    router: Router,
    uri: &str,
    payload: &T,
    principal: Option<Principal>,
) -> ServerResult<R>
where
    T: Serialize,
    R: DeserializeOwned,
{
    let response = router
        .oneshot(sync_request(uri, payload, principal))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

fn sync_request<T>(uri: &str, payload: &T, principal: Option<Principal>) -> Request<Body>
where
    T: Serialize,
{
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(principal) = principal {
        builder = builder.extension(principal);
    }
    builder
        .body(Body::from(serde_json::to_string(payload).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn open_and_pull_responses_carry_the_source_scope() {
    let scoped = ScopedStream::new();
    let sync = SyncServer::builder().public_stream(scoped).build();
    let router = finalize(sync);
    let principal = Principal::from_user(AuthUser::new("alice"));

    // /open stamps the per-stream scope the source derived from the
    // authenticated context.
    let open = SyncOpenRequest::new([SyncStreamName::new("scoped_stream").unwrap()]);
    let opened: ServerResult<SyncOpenResponse> = post_json(
        router.clone(),
        SYNC_OPEN_PATH,
        &open,
        Some(principal.clone()),
    )
    .await;
    let opened = opened.unwrap();
    assert_eq!(
        opened.streams[0].scope,
        Some(crate::SyncScope::new("user:alice").unwrap())
    );

    // /pull stamps the same scope onto the response (the source's
    // `pull` didn't set one itself).
    let pull = SyncPullRequest::new(SyncStreamName::new("scoped_stream").unwrap());
    let pulled: ServerResult<SyncPullResponse<Value>> =
        post_json(router.clone(), SYNC_PULL_PATH, &pull, Some(principal)).await;
    assert_eq!(
        pulled.unwrap().scope,
        Some(crate::SyncScope::new("user:alice").unwrap())
    );

    // An anonymous request resolves to no scope — the wire field is
    // simply absent and the client guard stays inert.
    let pulled: ServerResult<SyncPullResponse<Value>> =
        post_json(router, SYNC_PULL_PATH, &pull, None).await;
    assert!(pulled.unwrap().scope.is_none());
}

/// Stream whose `scope` derives from the authenticated principal —
/// the canonical shape for the cross-principal cache-clobber guard.
struct ScopedStream {
    stream: SyncStreamName,
    collection: SyncCollectionName,
}

impl ScopedStream {
    fn new() -> Self {
        Self {
            stream: SyncStreamName::new("scoped_stream").unwrap(),
            collection: SyncCollectionName::new("scoped").unwrap(),
        }
    }
}

impl SyncStreamSource for ScopedStream {
    fn stream(&self) -> &SyncStreamName {
        &self.stream
    }

    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }

    fn scope<'a>(&'a self, ctx: &'a RequestContext) -> SyncBoxFuture<'a, Option<crate::SyncScope>> {
        let user = ctx.principal().user().map(|user| user.id.clone());
        Box::pin(async move {
            user.map(|id| crate::SyncScope::new(format!("user:{id}")))
                .transpose()
        })
    }

    fn pull<'a>(
        &'a self,
        _ctx: RequestContext,
        _request: SyncPullRequest,
    ) -> SyncBoxFuture<'a, SyncPullResponse<Value>> {
        let stream = self.stream.clone();
        let collection = self.collection.clone();
        Box::pin(async move {
            Ok(SyncPullResponse::snapshot(
                stream,
                collection,
                Vec::new(),
                None,
            ))
        })
    }
}

struct ContextCaptureStream {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    seen: Arc<Mutex<Vec<String>>>,
}

impl ContextCaptureStream {
    fn new(seen: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            stream: SyncStreamName::new("context_stream").unwrap(),
            collection: SyncCollectionName::new("context").unwrap(),
            seen,
        }
    }
}

impl SyncStreamSource for ContextCaptureStream {
    fn stream(&self) -> &SyncStreamName {
        &self.stream
    }

    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }

    fn pull<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncBoxFuture<'a, SyncPullResponse<Value>> {
        let _ = request;
        let user_id = ctx
            .principal()
            .user()
            .map(|user| user.id.clone())
            .unwrap_or_else(|| "anonymous".to_string());
        self.seen.lock().unwrap().push(user_id.clone());
        Box::pin(async move {
            Ok(SyncPullResponse::snapshot(
                SyncStreamName::new("context_stream").unwrap(),
                SyncCollectionName::new("context").unwrap(),
                vec![SyncRow::new("ctx", json!(user_id))?],
                None,
            ))
        })
    }
}
