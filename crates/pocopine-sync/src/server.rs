use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use pocopine_core::{ServerError, ServerResult};
use pocopine_events::SharedEventBackend;
use pocopine_server::auth::{Predicate, RequestContext};
use pocopine_server::axum::body::Body;
use pocopine_server::axum::extract::{FromRequest, State};
use pocopine_server::axum::http::Request;
use pocopine_server::axum::response::Json;
use pocopine_server::axum::routing::post;
use pocopine_server::{Server, ServerPlugin};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    sync_stream_tag, MigrationOutcome, SyncError, SyncOpenRequest, SyncOpenResponse,
    SyncOpenStream, SyncPullRequest, SyncPullResponse, SyncPushRequest, SyncPushResponse,
    SyncResult, SYNC_OPEN_PATH, SYNC_PULL_PATH, SYNC_PUSH_PATH,
};

/// Future returned by a stream source.
pub type SyncBoxFuture<'a, T> = Pin<Box<dyn Future<Output = SyncResult<T>> + Send + 'a>>;

/// Future returned by a stream guard.
pub type SyncGuardFuture<'a> = Pin<Box<dyn Future<Output = ServerResult<()>> + Send + 'a>>;

/// Server-side access check for a sync stream.
pub trait SyncStreamGuard: Send + Sync + 'static {
    /// Authorize one request before the stream source runs.
    fn check(&self, ctx: RequestContext) -> SyncGuardFuture<'_>;
}

impl<F, Fut> SyncStreamGuard for F
where
    F: Fn(RequestContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ServerResult<()>> + Send + 'static,
{
    fn check(&self, ctx: RequestContext) -> SyncGuardFuture<'_> {
        Box::pin((self)(ctx))
    }
}

struct PredicateStreamGuard<P>(P);

impl<P> SyncStreamGuard for PredicateStreamGuard<P>
where
    P: Predicate,
{
    fn check(&self, ctx: RequestContext) -> SyncGuardFuture<'_> {
        let result: ServerResult<()> = self.0.check(&ctx.user).into();
        Box::pin(async move { result })
    }
}

/// Server-side source for one registered sync stream.
///
/// The request context is passed into each source call after stream-level
/// authorization has succeeded. Sources still own tenant filtering, cursor
/// validation, and any per-row visibility rules.
pub trait SyncStreamSource: Send + Sync + 'static {
    /// Server-registered stream name.
    fn stream(&self) -> &crate::SyncStreamName;

    /// Public collection name represented by this stream.
    fn collection(&self) -> &crate::SyncCollectionName;

    /// Current cursor, if the source can cheaply report it.
    fn current_cursor(&self, ctx: &RequestContext) -> Option<crate::SyncCursor> {
        let _ = ctx;
        None
    }

    /// Application-level schema version for this stream's row/draft shape.
    ///
    /// Bump this whenever the row, draft, or payload shape changes in a way
    /// that older cached data or queued mutations cannot deserialize into.
    /// The version is advertised on every `/open` response so the client
    /// can detect mismatches and either drop its cache + queue (default
    /// from `SyncLocalStore::clear_stream`) or migrate via the opt-in
    /// `migrate_payload` path on this trait (shipping in a follow-up).
    ///
    /// Defaults to `1` — apps that never bump never need to think about
    /// this. Authors using `#[resource]` set this through the
    /// `schema_version = N` attribute; bare `SyncStreamSource` impls
    /// override this method directly.
    fn schema_version(&self) -> u32 {
        crate::protocol::default_schema_version_one()
    }

    /// Pull changes or a snapshot for this stream.
    fn pull<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncBoxFuture<'a, SyncPullResponse<Value>>;

    /// Push client mutations for this stream.
    fn push<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'a, SyncPushResponse<Value>> {
        let _ = ctx;
        let _ = request;
        Box::pin(async {
            Err(SyncError::unsupported(
                "push is not implemented for this stream",
            ))
        })
    }

    /// Migrate a client-side mutation payload encoded under `from` to
    /// the source's currently-advertised schema version `to`.
    ///
    /// The framework's `push_handler` invokes this once per mutation
    /// when the request's `schema_version` is **strictly less than**
    /// `self.schema_version()`. A newer client pushing to an older
    /// server (rolling-deploy edge case) is NOT routed through
    /// migration — the source receives the newer-shape payload as-is.
    ///
    /// The default implementation returns `SyncError::SchemaMigration`
    /// so any out-of-tree source that hasn't opted in surfaces a clean
    /// per-mutation rejection rather than silently mis-deserializing
    /// stale payloads into the new shape. Apps that want to accept
    /// stale pushes (e.g. to preserve side-effecting writes like
    /// payments across a schema bump) override this method directly or
    /// use the `#[resource(schema_version = N, migrate_with = path)]`
    /// macro attribute which generates the impl.
    ///
    /// On `Ok`, the framework stores the migrated payload alongside
    /// the original (via `ClientMutation::migration_outcome`) so the
    /// source can deserialize the migrated value while still keying
    /// its idempotency log on the original wire payload. On `Err`,
    /// the framework stores the error reason in the same sidecar —
    /// the source surfaces it as a per-mutation rejection ONLY for
    /// mutations not already in its idempotency log (a retry of an
    /// already-accepted mutation succeeds even if the migrator now
    /// rejects). See `ClientMutation::take_processing_payload`.
    ///
    /// **Envelope constraint.** This method migrates the
    /// `serde_json::Value` payload only — it CANNOT migrate the
    /// outer `ClientMutation` (`key`, `op`, `base_version`). For
    /// changes to row-key shape (e.g. ID rename), prefer the
    /// default drop-the-cache path: bump `schema_version`, let the
    /// client wipe pending mutations + canonical rows, and rebuild
    /// from `/pull`.
    fn migrate_payload<'a>(&'a self, from: u32, to: u32, value: Value) -> SyncBoxFuture<'a, Value> {
        let _ = value;
        let stream = self.stream().as_str().to_string();
        Box::pin(async move { Err(SyncError::schema_migration(stream, from, to)) })
    }
}

#[derive(Clone)]
struct RegisteredSyncStream {
    source: Arc<dyn SyncStreamSource>,
    guard: Option<Arc<dyn SyncStreamGuard>>,
}

impl RegisteredSyncStream {
    async fn authorize(&self, ctx: RequestContext) -> ServerResult<()> {
        if let Some(guard) = &self.guard {
            guard.check(ctx).await
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
struct SyncServerInner {
    streams: Arc<HashMap<String, Arc<RegisteredSyncStream>>>,
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

    /// Return the live query tags this server can publish for streams.
    pub fn live_query_tags(&self) -> Vec<String> {
        self.inner
            .streams
            .keys()
            .map(|stream| sync_stream_tag(stream))
            .collect()
    }

    /// Return the live topics this server can publish for streams.
    pub fn live_topics(&self) -> pocopine_events::EventResult<Vec<pocopine_events::Topic>> {
        self.live_query_tags()
            .into_iter()
            .map(|tag| pocopine_live::query_tag_topic(&tag))
            .collect()
    }

    /// Publish a live wake-up for one stream when an event backend is
    /// attached. The sync data still moves through pull; this only wakes
    /// browsers to pull with their current sync cursor.
    pub async fn invalidate_stream(&self, stream: &str) -> SyncResult<()> {
        let Some(events) = self.inner.events.as_ref() else {
            return Ok(());
        };
        let tag = sync_stream_tag(stream);
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

    fn stream(&self, stream: &str) -> SyncResult<Arc<RegisteredSyncStream>> {
        self.inner
            .streams
            .get(stream)
            .cloned()
            .ok_or_else(|| SyncError::UnknownStream(stream.to_string()))
    }
}

/// Builder for [`SyncServer`].
#[derive(Default)]
pub struct SyncServerBuilder {
    streams: HashMap<String, Arc<RegisteredSyncStream>>,
    events: Option<SharedEventBackend>,
}

impl SyncServerBuilder {
    /// Register one explicitly public stream source.
    pub fn public_stream<S>(mut self, stream: S) -> Self
    where
        S: SyncStreamSource,
    {
        self.insert_stream(stream, None);
        self
    }

    /// Register one stream guarded by a sync auth predicate.
    pub fn guarded_stream<S, P>(mut self, stream: S, predicate: P) -> Self
    where
        S: SyncStreamSource,
        P: Predicate,
    {
        self.insert_stream(stream, Some(Arc::new(PredicateStreamGuard(predicate))));
        self
    }

    /// Register one stream guarded by an async request-context guard.
    pub fn guarded_stream_with<S, G>(mut self, stream: S, guard: G) -> Self
    where
        S: SyncStreamSource,
        G: SyncStreamGuard,
    {
        self.insert_stream(stream, Some(Arc::new(guard)));
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
                streams: Arc::new(self.streams),
                events: self.events,
            }),
        }
    }

    fn insert_stream<S>(&mut self, stream: S, guard: Option<Arc<dyn SyncStreamGuard>>)
    where
        S: SyncStreamSource,
    {
        self.streams.insert(
            stream.stream().as_str().to_string(),
            Arc::new(RegisteredSyncStream {
                source: Arc::new(stream),
                guard,
            }),
        );
    }
}

/// Server plugin that mounts sync routes and provides [`SyncServer`].
///
/// Installing this plugin exposes `/__pocopine/sync/v1/open`, `/pull`, and
/// `/push` for every registered stream. Streams must be registered as either
/// explicitly public or guarded; there is no implicit public registration.
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
    request: Request<Body>,
) -> Json<ServerResult<SyncOpenResponse>> {
    Json(
        async {
            let (ctx, request) = parse_json_request::<SyncOpenRequest>(request).await?;
            open(sync, ctx, request).await
        }
        .await,
    )
}

async fn open(
    sync: SyncServer,
    ctx: RequestContext,
    request: SyncOpenRequest,
) -> ServerResult<SyncOpenResponse> {
    let mut streams = Vec::with_capacity(request.streams.len());
    for requested in request.streams {
        let stream = sync.stream(requested.as_str()).map_err(server_error)?;
        stream.authorize(ctx.clone()).await?;
        streams.push(SyncOpenStream {
            stream: stream.source.stream().clone(),
            collection: stream.source.collection().clone(),
            cursor: stream.source.current_cursor(&ctx),
            schema_version: stream.source.schema_version(),
        });
    }
    Ok(SyncOpenResponse::new(streams))
}

async fn pull_handler(
    State(sync): State<SyncServer>,
    request: Request<Body>,
) -> Json<ServerResult<SyncPullResponse<Value>>> {
    let result = async {
        let (ctx, request) = parse_json_request::<SyncPullRequest>(request).await?;
        let stream = sync.stream(request.stream.as_str()).map_err(server_error)?;
        stream.authorize(ctx.clone()).await?;
        stream.source.pull(ctx, request).await.map_err(server_error)
    }
    .await;
    Json(result)
}

async fn push_handler(
    State(sync): State<SyncServer>,
    request: Request<Body>,
) -> Json<ServerResult<SyncPushResponse<Value>>> {
    let result = async {
        let (ctx, mut request) = parse_json_request::<SyncPushRequest<Value>>(request).await?;
        // Reject malformed wire payloads up-front: `schema_version == 0`
        // is not a valid version (versions start at 1) and the macro,
        // builder, and `/open` response validators all reject it. Match
        // here so a malicious or buggy client can't tunnel through
        // `migrate_payload` with `from = 0`.
        if request.schema_version == 0 {
            return Err(server_error(SyncError::invalid_value(
                "schema_version",
                "must be >= 1 (schema versions start at 1)",
            )));
        }
        let stream = sync.stream(request.stream.as_str()).map_err(server_error)?;
        stream.authorize(ctx.clone()).await?;
        let server_schema_version = stream.source.schema_version();
        // Reject `request.schema_version > server_schema_version` at
        // the wire layer. A newer client pushing to an older server
        // (rolling-deploy edge case) cannot safely fall through to
        // `source.push`: serde's default behaviour DROPS unknown
        // fields, so the older deserializer would silently accept a
        // newer-shape payload with fields missing. The client must
        // back off and retry; the durable queue stays intact thanks
        // to the typed error.
        if request.schema_version > server_schema_version {
            tracing::info!(
                target: "pocopine.log",
                stream = stream.source.stream().as_str(),
                request_schema_version = request.schema_version,
                server_schema_version,
                "sync push rejected: client schema_version newer than server (likely mid-deploy)"
            );
            return Err(server_error(SyncError::schema_migration(
                stream.source.stream().as_str().to_string(),
                request.schema_version,
                server_schema_version,
            )));
        }
        // When the client is on an OLDER schema version, attach a
        // per-mutation `migration_outcome` to each mutation. The
        // outcome is consumed by the source's `push`:
        //
        //   * `MigrationOutcome::Migrated(value)` — successful
        //     migration; the source applies the migrated value.
        //   * `MigrationOutcome::Failed { reason }` — the migrator
        //     rejected; the source surfaces the reason ONLY if its
        //     idempotency log doesn't already have this mutation_id+
        //     payload as accepted. This preserves replay-safety
        //     across migrator changes (e.g., a v1 mutation was
        //     accepted via a now-removed migrator; a retry from the
        //     same client hits the log and accepts despite the
        //     migrator no longer being registered).
        //
        // No pre-rejection happens at this layer — the source has
        // final say.
        if request.schema_version < server_schema_version {
            let from = request.schema_version;
            let to = server_schema_version;
            for mutation in request.mutations.iter_mut() {
                match stream
                    .source
                    .migrate_payload(from, to, mutation.payload.clone())
                    .await
                {
                    Ok(migrated) => {
                        mutation.migration_outcome = Some(MigrationOutcome::Migrated(migrated));
                    }
                    Err(err) => {
                        tracing::info!(
                            target: "pocopine.log",
                            stream = stream.source.stream().as_str(),
                            from,
                            to,
                            mutation_id = mutation.id.as_str(),
                            reason = %err,
                            "sync push migrate_payload failed; deferring to source idempotency check"
                        );
                        mutation.migration_outcome = Some(MigrationOutcome::Failed {
                            reason: err.to_string(),
                        });
                    }
                }
            }
            // NOTE: we deliberately leave `request.schema_version` as
            // the client's wire value. Custom `SyncStreamSource::push`
            // impls may consult it for logging/audit/version-aware
            // routing; rewriting it would silently lie about what the
            // client claimed.
        }
        let collection_name = stream.source.collection().clone();
        let mut response = stream
            .source
            .push(ctx, request)
            .await
            .map_err(server_error)?;
        if response.collection.is_none() {
            response.collection = Some(collection_name);
        }
        if !response.accepted.is_empty() {
            if let Err(err) = sync.invalidate_stream(response.stream.as_str()).await {
                tracing::warn!(
                    target: "pocopine.log",
                    error = %err,
                    stream = response.stream.as_str(),
                    "failed to publish sync stream invalidation after push"
                );
            }
        }
        Ok(response)
    }
    .await;
    Json(result)
}

async fn parse_json_request<T>(request: Request<Body>) -> ServerResult<(RequestContext, T)>
where
    T: DeserializeOwned + Send + 'static,
{
    let (parts, body) = request.into_parts();
    let ctx = RequestContext::from_parts(
        parts.method.clone(),
        parts.uri.clone(),
        parts.headers.clone(),
        parts.extensions.clone(),
    );
    let request = Request::from_parts(parts, body);
    let Json(payload) = Json::<T>::from_request(request, &())
        .await
        .map_err(|err| ServerError::BadRequest(err.to_string()))?;
    Ok((ctx, payload))
}

fn server_error(error: SyncError) -> ServerError {
    match error {
        SyncError::InvalidValue { .. } => ServerError::BadRequest(error.to_string()),
        SyncError::UnknownStream(_) => ServerError::Forbidden(error.to_string()),
        SyncError::Unsupported(_) => ServerError::BadRequest(error.to_string()),
        SyncError::Gap(_) => ServerError::BadRequest(error.to_string()),
        SyncError::SchemaMigration { .. } => ServerError::BadRequest(error.to_string()),
        SyncError::Unauthorized(msg) => ServerError::Unauthorized(msg),
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
    use std::sync::{Arc, Mutex};

    use http_body_util::BodyExt;
    use pocopine_core::ServerError;
    use pocopine_server::auth::{
        require_auth, require_role, AuthUser, Predicate, Principal, RequestContext, Role,
    };
    use pocopine_server::axum::body::Body;
    use pocopine_server::axum::http::{Request, StatusCode};
    use pocopine_server::axum::Router;
    use serde::de::DeserializeOwned;
    use serde::Serialize;
    use serde_json::{json, Value};
    use tower::ServiceExt;

    use super::*;
    use crate::{
        ClientMutation, MemorySyncStream, MutationId, RowKey, SyncCollectionName, SyncOp,
        SyncOpenRequest, SyncPullMode, SyncPushRequest, SyncRow, SyncStreamName, SYNC_PROTOCOL_V1,
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
                .user
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
}
