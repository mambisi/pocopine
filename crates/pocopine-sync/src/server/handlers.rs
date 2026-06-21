use pocopine_core::{ServerError, ServerResult};
use pocopine_server::auth::RequestContext;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::extract::{FromRequest, State};
use pocopine_server::axum::http::Request;
use pocopine_server::axum::response::Json;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::*;
use crate::{
    MigrationOutcome, SyncError, SyncOpenRequest, SyncOpenResponse, SyncOpenStream,
    SyncPullRequest, SyncPullResponse, SyncPushRequest, SyncPushResponse,
};

pub(crate) async fn open_handler(
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
        let stream = sync
            .stream(requested.stream.as_str())
            .map_err(server_error)?;
        stream.authorize(ctx.clone()).await?;
        // Run the source's `validate_params` after authorization so
        // params logic cannot be abused to bypass auth. Default impl
        // accepts anything; sources declaring `params(...)` via the
        // macro get a strongly-typed validator that rejects unknown
        // keys + wrong types.
        stream
            .source
            .validate_params(&requested.params)
            .map_err(server_error)?;
        streams.push(SyncOpenStream {
            stream: stream.source.stream().clone(),
            collection: stream.source.collection().clone(),
            cursor: stream.source.current_cursor(&ctx),
            schema_version: stream.source.schema_version(),
            params: requested.params,
        });
    }
    Ok(SyncOpenResponse::new(streams))
}

pub(crate) async fn pull_handler(
    State(sync): State<SyncServer>,
    request: Request<Body>,
) -> Json<ServerResult<SyncPullResponse<Value>>> {
    let result = async {
        let (ctx, request) = parse_json_request::<SyncPullRequest>(request).await?;
        let stream = sync.stream(request.stream.as_str()).map_err(server_error)?;
        stream.authorize(ctx.clone()).await?;
        // Pull-time params validation is ALWAYS strict (unlike push,
        // which is loose-on-empty). The /pull endpoint returns rows;
        // a parameterized source that lets an empty-params pull
        // through would return the unfiltered, auth-scoped view —
        // which is broader than any single subscription's filter and
        // is an exfiltration vector for shape-aware tenancy
        // (workspace_id, channel_id, etc). Sources that want to
        // accept empty-params pulls (e.g. CRUD post-push
        // reconciliation pulls) MUST override `validate_params` to
        // make the empty case explicit.
        stream
            .source
            .validate_params(&request.params)
            .map_err(server_error)?;
        stream.source.pull(ctx, request).await.map_err(server_error)
    }
    .await;
    Json(result)
}

pub(crate) async fn push_handler(
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
        // Push-time params validation routes through the dedicated
        // `validate_push_params` trait method. Default impl delegates
        // to `validate_params` (strict). CRUD-style sources override
        // to accept the empty-params CRUD write case explicitly —
        // shape-aware out-of-tree sources keep the strict gate.
        stream
            .source
            .validate_push_params(&request.params)
            .map_err(server_error)?;
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
            // BARE topic — one publish per push, regardless of how
            // many rows were accepted or returned. This preserves
            // the pre-RFC 088 §C behavior for delete-only pushes
            // (where `response.rows` may be empty even though
            // `accepted` is non-empty) and stops old clients on the
            // bare topic from receiving N redundant wakeups for a
            // multi-row push.
            if let Err(err) = sync.invalidate_stream(response.stream.as_str()).await {
                tracing::warn!(
                    target: "pocopine.log",
                    error = %err,
                    stream = response.stream.as_str(),
                    "failed to publish bare sync stream invalidation after push"
                );
            }

            // PER-PARAMS topics — batch all accepted rows, project
            // each through `row_to_params`, and publish ONCE per
            // distinct hash (multiple rows hitting the same
            // partition collapse to one wakeup). Sources that don't
            // override `row_to_params` (default impl returns empty)
            // make this a no-op via the empty-params guard inside
            // the helper. Sources that DO override route precisely
            // to the audience whose subscription params project to
            // the same hash (RFC 088 §C). Pushes that accepted but
            // returned no rows (deletes that don't echo the removed
            // row) get bare-only invalidation — that's the right
            // semantic since per-params routing needs a row to
            // project.
            // Collect into a `Vec<&Value>` rather than passing the
            // lazy `map(|r| &r.value)` iterator directly. The map
            // closure infers a single concrete lifetime for its
            // argument, which trips the route-handler's HRTB FnOnce
            // bound (axum needs `for<'r>` generality). Collecting
            // sidesteps the HRTB by handing
            // `invalidate_stream_with_rows` an already-erased
            // iterator type.
            let row_values: ::std::vec::Vec<&Value> =
                response.rows.iter().map(|r| &r.value).collect();
            if let Err(err) = sync
                .invalidate_stream_with_rows(response.stream.as_str(), row_values)
                .await
            {
                tracing::warn!(
                    target: "pocopine.log",
                    error = %err,
                    stream = response.stream.as_str(),
                    "failed to publish per-params sync stream invalidations after push"
                );
            }
        }
        Ok(response)
    }
    .await;
    Json(result)
}

pub(crate) async fn parse_json_request<T>(
    request: Request<Body>,
) -> ServerResult<(RequestContext, T)>
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

pub(crate) fn server_error(error: SyncError) -> ServerError {
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
        SyncError::Network(msg) => {
            // Server-side handlers don't generate Network errors —
            // that variant is for the sync-query driver classifying
            // transport failures from a `Mutator::apply_remote`
            // future. If we ever observe one here it's a bug; map
            // to a generic 500 with the message in the log.
            tracing::error!(
                target: "pocopine.log",
                error = %msg,
                "unexpected SyncError::Network surfaced from a sync request handler"
            );
            ServerError::App("sync internal error".to_string())
        }
    }
}
