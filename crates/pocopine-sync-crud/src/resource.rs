use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use pocopine_auth::RequestContext;
use pocopine_sync::{
    MutationId, RowVersion, SyncBoxFuture, SyncCollectionName, SyncConflict, SyncError,
    SyncPullRequest, SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncRejectedMutation,
    SyncResult, SyncRow, SyncStreamName, SyncStreamSource,
};
use serde::Serialize;
use serde_json::Value;

use crate::{CrudMutationPayload, CrudSource, ResourceId};

/// Start building a CRUD-backed sync stream.
///
/// The `name` is used as both the sync stream and collection name. The builder
/// does not generate SQL; it only wires a [`CrudSource`] into Pocopine sync.
pub fn resource<S>(name: impl Into<String>, source: S) -> SyncResult<CrudResourceBuilder<S>>
where
    S: CrudSource,
{
    let name = name.into();
    Ok(CrudResourceBuilder {
        stream: SyncStreamName::new(name.clone())?,
        collection: SyncCollectionName::new(name)?,
        source,
    })
}

/// Builder returned by [`resource`].
pub struct CrudResourceBuilder<S> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    source: S,
}

impl<S> CrudResourceBuilder<S>
where
    S: CrudSource,
{
    /// Attach the row id extractor for this resource.
    pub fn id<IdOf>(self, id_of: IdOf) -> CrudResource<S, IdOf>
    where
        IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    {
        CrudResource {
            stream: self.stream,
            collection: self.collection,
            source: self.source,
            id_of,
            version_of: NoRowVersion,
            mutation_log: MissingMutationLog,
        }
    }
}

/// CRUD resource adapter that can be registered with `pocopine-sync`.
pub struct CrudResource<S, IdOf, VersionOf = NoRowVersion, Log = MissingMutationLog> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    source: S,
    id_of: IdOf,
    version_of: VersionOf,
    mutation_log: Log,
}

impl<S, IdOf, VersionOf, Log> CrudResource<S, IdOf, VersionOf, Log>
where
    S: CrudSource,
{
    /// Attach a row version extractor used for base-version conflict checks.
    pub fn version<NextVersionOf, Version>(
        self,
        version_of: NextVersionOf,
    ) -> CrudResource<S, IdOf, NextVersionOf, Log>
    where
        NextVersionOf: Fn(&S::Row) -> Version + Send + Sync + 'static,
        Version: RowVersionValue,
    {
        CrudResource {
            stream: self.stream,
            collection: self.collection,
            source: self.source,
            id_of: self.id_of,
            version_of,
            mutation_log: self.mutation_log,
        }
    }
}

impl<S, IdOf, VersionOf> CrudResource<S, IdOf, VersionOf, MissingMutationLog>
where
    S: CrudSource,
{
    /// Attach a mutation log used to dedupe replayed accepted mutations.
    ///
    /// Production logs should be backed by the same database as the source and
    /// recorded in the same transaction as the row write.
    pub fn mutation_log<Log>(self, mutation_log: Log) -> CrudResource<S, IdOf, VersionOf, Log>
    where
        Log: CrudMutationLog<S::Row>,
    {
        CrudResource {
            stream: self.stream,
            collection: self.collection,
            source: self.source,
            id_of: self.id_of,
            version_of: self.version_of,
            mutation_log,
        }
    }

    /// Attach a process-local mutation log for tests and single-process demos.
    ///
    /// This is not a production idempotency backend because it is not durable.
    pub fn memory_mutation_log(
        self,
    ) -> CrudResource<S, IdOf, VersionOf, MemoryCrudMutationLog<S::Row>> {
        self.mutation_log(MemoryCrudMutationLog::new())
    }
}

/// Marker used before a resource has an idempotency backend.
pub struct MissingMutationLog;

/// Marker used when a resource does not expose row versions.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoRowVersion;

/// Convert a row version extractor value into an optional sync row version.
pub trait RowVersionValue {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>>;
}

impl RowVersionValue for RowVersion {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        Ok(Some(self))
    }
}

impl RowVersionValue for Option<RowVersion> {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        Ok(self)
    }
}

impl RowVersionValue for String {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        Ok(Some(RowVersion::new(self)?))
    }
}

impl RowVersionValue for Option<String> {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        self.map(RowVersion::new).transpose()
    }
}

impl RowVersionValue for &str {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        Ok(Some(RowVersion::new(self)?))
    }
}

impl RowVersionValue for Option<&str> {
    fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
        self.map(RowVersion::new).transpose()
    }
}

macro_rules! integer_row_version {
    ($($ty:ty),* $(,)?) => {
        $(
            impl RowVersionValue for $ty {
                fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
                    Ok(Some(RowVersion::new(self.to_string())?))
                }
            }

            impl RowVersionValue for Option<$ty> {
                fn into_row_version(self) -> SyncResult<Option<RowVersion>> {
                    self.map(|version| RowVersion::new(version.to_string())).transpose()
                }
            }
        )*
    };
}

integer_row_version!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

#[doc(hidden)]
pub trait RowVersionOf<Row>: Send + Sync + 'static {
    fn tracks_versions(&self) -> bool;
    fn row_version(&self, row: &Row) -> SyncResult<Option<RowVersion>>;
}

impl<Row> RowVersionOf<Row> for NoRowVersion {
    fn tracks_versions(&self) -> bool {
        false
    }

    fn row_version(&self, row: &Row) -> SyncResult<Option<RowVersion>> {
        let _ = row;
        Ok(None)
    }
}

impl<Row, F, V> RowVersionOf<Row> for F
where
    F: Fn(&Row) -> V + Send + Sync + 'static,
    V: RowVersionValue,
{
    fn tracks_versions(&self) -> bool {
        true
    }

    fn row_version(&self, row: &Row) -> SyncResult<Option<RowVersion>> {
        (self)(row).into_row_version()
    }
}

/// Accepted mutation entry stored by a [`CrudMutationLog`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrudAcceptedMutation<Row> {
    pub mutation_id: MutationId,
    pub row: Option<SyncRow<Row>>,
}

impl<Row> CrudAcceptedMutation<Row> {
    pub fn new(mutation_id: MutationId, row: Option<SyncRow<Row>>) -> Self {
        Self { mutation_id, row }
    }
}

/// Idempotency log for CRUD mutations.
#[async_trait::async_trait]
pub trait CrudMutationLog<Row>: Send + Sync + 'static
where
    Row: Clone + Send + Sync + 'static,
{
    async fn accepted_mutation(
        &self,
        ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<CrudAcceptedMutation<Row>>>;

    async fn record_accepted_mutation(
        &self,
        ctx: &RequestContext,
        accepted: CrudAcceptedMutation<Row>,
    ) -> SyncResult<()>;
}

/// Process-local mutation log for tests and single-process demos.
#[derive(Clone, Debug)]
pub struct MemoryCrudMutationLog<Row> {
    accepted: Arc<Mutex<BTreeMap<String, CrudAcceptedMutation<Row>>>>,
}

impl<Row> Default for MemoryCrudMutationLog<Row> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Row> MemoryCrudMutationLog<Row> {
    pub fn new() -> Self {
        Self {
            accepted: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl<Row> CrudMutationLog<Row> for MemoryCrudMutationLog<Row>
where
    Row: Clone + Send + Sync + 'static,
{
    async fn accepted_mutation(
        &self,
        ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<CrudAcceptedMutation<Row>>> {
        let _ = ctx;
        let accepted = self
            .accepted
            .lock()
            .map_err(|_| SyncError::backend("memory CRUD mutation log lock poisoned"))?;
        Ok(accepted.get(mutation_id.as_str()).cloned())
    }

    async fn record_accepted_mutation(
        &self,
        ctx: &RequestContext,
        accepted: CrudAcceptedMutation<Row>,
    ) -> SyncResult<()> {
        let _ = ctx;
        let mut entries = self
            .accepted
            .lock()
            .map_err(|_| SyncError::backend("memory CRUD mutation log lock poisoned"))?;
        entries.insert(accepted.mutation_id.as_str().to_string(), accepted);
        Ok(())
    }
}

impl<S, IdOf, VersionOf, Log> SyncStreamSource for CrudResource<S, IdOf, VersionOf, Log>
where
    S: CrudSource,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    VersionOf: RowVersionOf<S::Row>,
    Log: CrudMutationLog<S::Row>,
{
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
        Box::pin(async move { self.pull_snapshot(ctx, request).await })
    }

    fn push<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'a, SyncPushResponse<Value>> {
        Box::pin(async move { self.push_mutations(ctx, request).await })
    }
}

impl<S, IdOf, VersionOf, Log> CrudResource<S, IdOf, VersionOf, Log>
where
    S: CrudSource,
    IdOf: Fn(&S::Row) -> S::Id + Send + Sync + 'static,
    VersionOf: RowVersionOf<S::Row>,
    Log: CrudMutationLog<S::Row>,
{
    async fn pull_snapshot(
        &self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncResult<SyncPullResponse<Value>> {
        if request.stream != self.stream {
            return Err(SyncError::UnknownStream(request.stream.to_string()));
        }

        let rows = self.source.list(ctx).await?;
        let rows = rows
            .into_iter()
            .map(|row| self.row_to_value(row))
            .collect::<SyncResult<Vec<_>>>()?;

        Ok(SyncPullResponse::snapshot(
            self.stream.clone(),
            self.collection.clone(),
            rows,
            None,
        ))
    }

    async fn push_mutations(
        &self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncResult<SyncPushResponse<Value>> {
        if request.stream != self.stream {
            return Err(SyncError::UnknownStream(request.stream.to_string()));
        }

        let mut response = SyncPushResponse::new(self.stream.clone());
        response.collection = Some(self.collection.clone());

        for mutation in request.mutations {
            if let Some(accepted) = self
                .mutation_log
                .accepted_mutation(&ctx, &mutation.id)
                .await?
            {
                response.accepted.push(mutation.id);
                if let Some(row) = accepted.row {
                    response.rows.push(row_to_value(row)?);
                }
                continue;
            }

            let mutation_id = mutation.id.clone();
            let key = mutation.key.clone();
            let payload = match serde_json::from_value::<CrudMutationPayload<S::Id, S::Draft>>(
                mutation.payload,
            ) {
                Ok(payload) => payload,
                Err(err) => {
                    response.rejected.push(SyncRejectedMutation {
                        mutation_id,
                        key,
                        reason: format!("invalid CRUD mutation payload: {err}"),
                    });
                    continue;
                }
            };

            if payload.sync_op() != mutation.op {
                response.rejected.push(SyncRejectedMutation {
                    mutation_id,
                    key,
                    reason: "CRUD payload does not match sync operation".to_string(),
                });
                continue;
            }

            let expected_key = payload.id().to_row_key()?;
            if mutation.key.as_ref() != Some(&expected_key) {
                response.rejected.push(SyncRejectedMutation {
                    mutation_id,
                    key: mutation.key,
                    reason: "CRUD mutation row key does not match payload id".to_string(),
                });
                continue;
            }

            match self
                .apply_payload(
                    &ctx,
                    mutation_id,
                    expected_key,
                    mutation.base_version,
                    payload,
                )
                .await?
            {
                CrudApplyOutcome::Accepted { mutation_id, row } => {
                    self.mutation_log
                        .record_accepted_mutation(
                            &ctx,
                            CrudAcceptedMutation::new(mutation_id.clone(), row.clone()),
                        )
                        .await?;
                    response.accepted.push(mutation_id);
                    if let Some(row) = row {
                        response.rows.push(row_to_value(row)?);
                    }
                }
                CrudApplyOutcome::Rejected(rejected) => response.rejected.push(rejected),
                CrudApplyOutcome::Conflict(conflict) => {
                    response.conflicts.push(conflict_to_value(conflict)?);
                }
            }
        }

        Ok(response)
    }

    async fn apply_payload(
        &self,
        ctx: &RequestContext,
        mutation_id: MutationId,
        key: pocopine_sync::RowKey,
        base_version: Option<RowVersion>,
        payload: CrudMutationPayload<S::Id, S::Draft>,
    ) -> SyncResult<CrudApplyOutcome<S::Row>> {
        match payload {
            CrudMutationPayload::Create(payload) => {
                if base_version.is_some() {
                    return Ok(CrudApplyOutcome::Rejected(SyncRejectedMutation {
                        mutation_id,
                        key: Some(key),
                        reason: "create does not accept a base row version".to_string(),
                    }));
                }

                let row = self
                    .source
                    .create(ctx.clone(), payload.id, payload.draft)
                    .await?;
                let row = self.row_to_sync(row)?;
                Ok(CrudApplyOutcome::Accepted {
                    mutation_id,
                    row: Some(row),
                })
            }
            CrudMutationPayload::Save(payload) => {
                if let Some(conflict) = self
                    .conflict_for_base_version(
                        ctx,
                        mutation_id.clone(),
                        Some(key.clone()),
                        payload.id.clone(),
                        base_version,
                    )
                    .await?
                {
                    return Ok(CrudApplyOutcome::Conflict(conflict));
                }

                let row = self
                    .source
                    .save(ctx.clone(), payload.id, payload.draft)
                    .await?;
                let row = self.row_to_sync(row)?;
                Ok(CrudApplyOutcome::Accepted {
                    mutation_id,
                    row: Some(row),
                })
            }
            CrudMutationPayload::Remove(payload) => {
                if let Some(conflict) = self
                    .conflict_for_base_version(
                        ctx,
                        mutation_id.clone(),
                        Some(key.clone()),
                        payload.id.clone(),
                        base_version,
                    )
                    .await?
                {
                    return Ok(CrudApplyOutcome::Conflict(conflict));
                }

                self.source.remove(ctx.clone(), payload.id).await?;
                Ok(CrudApplyOutcome::Accepted {
                    mutation_id,
                    row: None,
                })
            }
        }
    }

    async fn conflict_for_base_version(
        &self,
        ctx: &RequestContext,
        mutation_id: MutationId,
        key: Option<pocopine_sync::RowKey>,
        id: S::Id,
        base_version: Option<RowVersion>,
    ) -> SyncResult<Option<SyncConflict<S::Row>>> {
        let Some(base_version) = base_version else {
            return Ok(None);
        };

        if !self.version_of.tracks_versions() {
            return Ok(Some(SyncConflict {
                mutation_id,
                key,
                server_row: None,
                reason: "base version requires a CRUD resource version mapper".to_string(),
            }));
        }

        let server_row = self.source.get(ctx.clone(), id).await?;
        let Some(server_row) = server_row else {
            return Ok(Some(SyncConflict {
                mutation_id,
                key,
                server_row: None,
                reason: "base version is stale".to_string(),
            }));
        };

        let server_version = self.version_of.row_version(&server_row)?;
        if server_version.as_ref() == Some(&base_version) {
            return Ok(None);
        }

        Ok(Some(SyncConflict {
            mutation_id,
            key,
            server_row: Some(self.row_to_sync(server_row)?),
            reason: "base version is stale".to_string(),
        }))
    }

    fn row_to_sync(&self, row: S::Row) -> SyncResult<SyncRow<S::Row>> {
        let key = (self.id_of)(&row).to_row_key()?;
        let version = self.version_of.row_version(&row)?;
        Ok(SyncRow {
            key,
            version,
            value: row,
            pending: false,
            conflict: false,
        })
    }

    fn row_to_value(&self, row: S::Row) -> SyncResult<SyncRow<Value>> {
        row_to_value(self.row_to_sync(row)?)
    }
}

enum CrudApplyOutcome<Row> {
    Accepted {
        mutation_id: MutationId,
        row: Option<SyncRow<Row>>,
    },
    Rejected(SyncRejectedMutation),
    Conflict(SyncConflict<Row>),
}

fn row_to_value<Row>(row: SyncRow<Row>) -> SyncResult<SyncRow<Value>>
where
    Row: Serialize,
{
    Ok(SyncRow {
        key: row.key,
        version: row.version,
        value: serde_json::to_value(row.value)?,
        pending: row.pending,
        conflict: row.conflict,
    })
}

fn conflict_to_value<Row>(conflict: SyncConflict<Row>) -> SyncResult<SyncConflict<Value>>
where
    Row: Serialize,
{
    Ok(SyncConflict {
        mutation_id: conflict.mutation_id,
        key: conflict.key,
        server_row: conflict.server_row.map(row_to_value).transpose()?,
        reason: conflict.reason,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use http::{HeaderMap, Method, Uri};
    use pocopine_sync::{ClientMutation, SyncPullMode};
    use serde::{Deserialize, Serialize};

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Post {
        id: String,
        title: String,
        version: u64,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct PostDraft {
        title: String,
    }

    #[derive(Clone, Default)]
    struct Posts {
        rows: Arc<Mutex<BTreeMap<String, Post>>>,
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl Posts {
        fn insert(&self, post: Post) {
            self.rows.lock().unwrap().insert(post.id.clone(), post);
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        fn record(&self, call: impl Into<String>) {
            self.calls.lock().unwrap().push(call.into());
        }
    }

    #[async_trait::async_trait]
    impl CrudSource for Posts {
        type Id = String;
        type Row = Post;
        type Draft = PostDraft;

        async fn list(&self, ctx: RequestContext) -> SyncResult<Vec<Self::Row>> {
            let _ = ctx;
            let rows = self.rows.lock().unwrap();
            Ok(rows.values().cloned().collect())
        }

        async fn get(&self, ctx: RequestContext, id: Self::Id) -> SyncResult<Option<Self::Row>> {
            let _ = ctx;
            Ok(self.rows.lock().unwrap().get(&id).cloned())
        }

        async fn create(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            draft: Self::Draft,
        ) -> SyncResult<Self::Row> {
            let _ = ctx;
            self.record(format!("create:{id}"));
            let post = Post {
                id: id.clone(),
                title: draft.title,
                version: 1,
            };
            self.rows.lock().unwrap().insert(id, post.clone());
            Ok(post)
        }

        async fn save(
            &self,
            ctx: RequestContext,
            id: Self::Id,
            draft: Self::Draft,
        ) -> SyncResult<Self::Row> {
            let _ = ctx;
            self.record(format!("save:{id}"));
            let mut rows = self.rows.lock().unwrap();
            let row = rows
                .get_mut(&id)
                .ok_or_else(|| SyncError::backend("missing post"))?;
            row.title = draft.title;
            row.version += 1;
            Ok(row.clone())
        }

        async fn remove(&self, ctx: RequestContext, id: Self::Id) -> SyncResult<()> {
            let _ = ctx;
            self.record(format!("remove:{id}"));
            self.rows.lock().unwrap().remove(&id);
            Ok(())
        }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(Method::GET, Uri::from_static("/sync"), HeaderMap::new())
    }

    fn posts_resource(
        posts: Posts,
    ) -> CrudResource<
        Posts,
        impl Fn(&Post) -> String + Send + Sync + 'static,
        impl Fn(&Post) -> u64 + Send + Sync + 'static,
        MemoryCrudMutationLog<Post>,
    > {
        resource("posts", posts)
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .version(|post: &Post| post.version)
            .memory_mutation_log()
    }

    fn value_mutation(
        mutation: pocopine_sync::ClientMutation<CrudMutationPayload<String, PostDraft>>,
    ) -> pocopine_sync::ClientMutation<Value> {
        pocopine_sync::ClientMutation {
            id: mutation.id,
            key: mutation.key,
            op: mutation.op,
            base_version: mutation.base_version,
            payload: serde_json::to_value(mutation.payload).unwrap(),
        }
    }

    fn push_request(
        mutations: impl IntoIterator<
            Item = pocopine_sync::ClientMutation<CrudMutationPayload<String, PostDraft>>,
        >,
    ) -> SyncPushRequest<Value> {
        SyncPushRequest::new(
            SyncStreamName::new("posts").unwrap(),
            mutations.into_iter().map(value_mutation),
        )
    }

    #[tokio::test]
    async fn pull_returns_snapshot_rows() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "hello".to_string(),
            version: 7,
        });
        let resource = posts_resource(posts);

        let response = resource
            .pull(
                ctx(),
                SyncPullRequest::new(SyncStreamName::new("posts").unwrap()),
            )
            .await
            .unwrap();

        assert_eq!(response.mode, SyncPullMode::Snapshot);
        assert_eq!(response.rows.len(), 1);
        assert_eq!(response.rows[0].key.as_str(), "post_1");
        assert_eq!(response.rows[0].version.as_ref().unwrap().as_str(), "7");
        assert_eq!(response.rows[0].value["title"], "hello");
    }

    #[tokio::test]
    async fn push_routes_create_save_and_remove() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "old".to_string(),
            version: 1,
        });
        let resource = posts_resource(posts.clone());

        let create = CrudMutationPayload::create(
            "post_2".to_string(),
            PostDraft {
                title: "created".to_string(),
            },
        );
        let save = CrudMutationPayload::save(
            "post_1".to_string(),
            PostDraft {
                title: "saved".to_string(),
            },
        );
        let remove: CrudMutationPayload<String, PostDraft> =
            CrudMutationPayload::remove("post_2".to_string());
        let mutations = vec![
            create
                .into_sync_draft()
                .unwrap()
                .with_id(MutationId::new("device_1:1").unwrap()),
            save.into_sync_draft_with_base_version(Some(RowVersion::new("1").unwrap()))
                .unwrap()
                .with_id(MutationId::new("device_1:2").unwrap()),
            remove
                .into_sync_draft()
                .unwrap()
                .with_id(MutationId::new("device_1:3").unwrap()),
        ];

        let response = resource.push(ctx(), push_request(mutations)).await.unwrap();

        assert_eq!(response.accepted.len(), 3);
        assert_eq!(response.rows.len(), 2);
        assert!(response.rejected.is_empty());
        assert!(response.conflicts.is_empty());
        assert_eq!(
            posts.calls(),
            vec!["create:post_2", "save:post_1", "remove:post_2"]
        );
    }

    #[tokio::test]
    async fn duplicate_accepted_mutation_does_not_reapply_source_write() {
        let posts = Posts::default();
        let resource = posts_resource(posts.clone());
        let payload = CrudMutationPayload::create(
            "post_1".to_string(),
            PostDraft {
                title: "hello".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft()
            .unwrap()
            .with_id(MutationId::new("device_1:1").unwrap());

        resource
            .push(ctx(), push_request([mutation.clone()]))
            .await
            .unwrap();
        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();

        assert_eq!(response.accepted.len(), 1);
        assert_eq!(response.rows.len(), 1);
        assert_eq!(posts.calls(), vec!["create:post_1"]);
    }

    #[tokio::test]
    async fn bad_payload_is_rejected_per_mutation() {
        let posts = Posts::default();
        let resource = posts_resource(posts);
        let mutation = ClientMutation::upsert(
            MutationId::new("device_1:bad").unwrap(),
            serde_json::json!({ "op": "create", "payload": { "id": "post_1" } }),
        )
        .key("post_1")
        .unwrap();

        let response = resource
            .push(
                ctx(),
                SyncPushRequest::new(SyncStreamName::new("posts").unwrap(), [mutation]),
            )
            .await
            .unwrap();

        assert_eq!(response.rejected.len(), 1);
        assert!(response.rejected[0]
            .reason
            .contains("invalid CRUD mutation payload"));
    }

    #[tokio::test]
    async fn stale_base_version_returns_conflict() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "server".to_string(),
            version: 2,
        });
        let resource = posts_resource(posts.clone());
        let payload = CrudMutationPayload::save(
            "post_1".to_string(),
            PostDraft {
                title: "client".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft_with_base_version(Some(RowVersion::new("1").unwrap()))
            .unwrap()
            .with_id(MutationId::new("device_1:stale").unwrap());

        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();

        assert_eq!(response.conflicts.len(), 1);
        assert_eq!(response.conflicts[0].reason, "base version is stale");
        assert!(response.accepted.is_empty());
        assert!(posts.calls().is_empty());
    }

    #[tokio::test]
    async fn base_version_without_version_mapper_conflicts_before_write() {
        let posts = Posts::default();
        posts.insert(Post {
            id: "post_1".to_string(),
            title: "server".to_string(),
            version: 2,
        });
        let resource = resource("posts", posts.clone())
            .unwrap()
            .id(|post: &Post| post.id.clone())
            .memory_mutation_log();
        let payload = CrudMutationPayload::save(
            "post_1".to_string(),
            PostDraft {
                title: "client".to_string(),
            },
        );
        let mutation = payload
            .into_sync_draft_with_base_version(Some(RowVersion::new("1").unwrap()))
            .unwrap()
            .with_id(MutationId::new("device_1:no_version").unwrap());

        let response = resource
            .push(ctx(), push_request([mutation]))
            .await
            .unwrap();

        assert_eq!(response.conflicts.len(), 1);
        assert!(response.conflicts[0]
            .reason
            .contains("requires a CRUD resource version mapper"));
        assert!(posts.calls().is_empty());
    }
}
