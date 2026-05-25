use pocopine_sync::{MutationId, SyncCollection, SyncPushResponse, SyncResult, SyncRow};
use std::marker::PhantomData;

use crate::{
    optimistic_row, CreateOptions, CrudMutationPayload, CrudOutcome, LocalResourceView,
    RemoveOptions, ResourceId, SaveOptions, WritePolicy,
};

/// Non-macro client runtime for one CRUD resource.
///
/// Generated resource modules should target this layer instead of manually
/// constructing sync protocol envelopes.
pub struct CrudClientResource<C: 'static, Id, Row> {
    collection: SyncCollection<C, Row>,
    view: LocalResourceView<Id, Row>,
    _marker: PhantomData<fn(C) -> (Id, Row)>,
}

/// Build a non-macro CRUD client resource from a sync collection and typed
/// local resource view.
pub fn client_resource<C: 'static, Id, Row>(
    collection: SyncCollection<C, Row>,
    view: LocalResourceView<Id, Row>,
) -> CrudClientResource<C, Id, Row> {
    CrudClientResource {
        collection,
        view,
        _marker: PhantomData,
    }
}

impl<C, Id, Row> CrudClientResource<C, Id, Row>
where
    C: 'static,
    Id: ResourceId,
    Row: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    /// Queue or send a create mutation with default options.
    pub async fn create<Draft>(self, id: Id, draft: Draft) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        self.create_with_options(id, draft, CreateOptions::default())
            .await
    }

    /// Queue or send a create mutation with explicit options.
    pub async fn create_with_options<Draft>(
        self,
        id: Id,
        draft: Draft,
        options: CreateOptions<Row>,
    ) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        let optimistic = options
            .optimistic
            .map(|row| optimistic_row(&id, row))
            .transpose()?;
        let payload = CrudMutationPayload::create(id.clone(), draft);
        self.send(id, payload, optimistic, options.write_policy, false)
            .await
    }

    /// Queue or send a save mutation with default options.
    pub async fn save<Draft>(self, id: Id, draft: Draft) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        self.save_with_options(id, draft, SaveOptions::default())
            .await
    }

    /// Queue or send a save mutation with explicit options.
    ///
    /// When `base_version` is not supplied explicitly, this method reads the
    /// latest canonical version from `LocalResourceView`. If the same row has
    /// pending local writes, a later save may still conflict on the server; the
    /// default is optimistic concurrency against the last confirmed server row,
    /// not a silent merge across queued edits.
    pub async fn save_with_options<Draft>(
        self,
        id: Id,
        draft: Draft,
        options: SaveOptions<Row>,
    ) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        let base_version = options
            .base_version
            .or_else(|| self.view.base_version(&id).cloned());
        let optimistic = options
            .optimistic
            .map(|row| optimistic_row(&id, row))
            .transpose()?;
        let payload = CrudMutationPayload::save(id.clone(), draft);
        let draft = payload.into_sync_draft_with_base_version(base_version)?;
        self.send_draft(id, draft, optimistic, options.write_policy, false)
            .await
    }

    /// Queue or send a remove mutation with default options.
    pub async fn remove(self, id: Id) -> SyncResult<CrudOutcome<Id, Row>> {
        self.remove_with_options(id, RemoveOptions::default()).await
    }

    /// Queue or send a remove mutation with explicit options.
    ///
    /// Like save, the default `base_version` comes from the latest canonical
    /// row in `LocalResourceView`; pending local writes for the same id are not
    /// collapsed into a synthetic version.
    pub async fn remove_with_options(
        self,
        id: Id,
        options: RemoveOptions,
    ) -> SyncResult<CrudOutcome<Id, Row>> {
        let base_version = options
            .base_version
            .or_else(|| self.view.base_version(&id).cloned());
        let payload: CrudMutationPayload<Id, ()> = CrudMutationPayload::remove(id.clone());
        let draft = payload.into_sync_draft_with_base_version(base_version)?;
        self.send_draft(id, draft, None, options.write_policy, true)
            .await
    }

    /// Resolve a conflict by accepting the currently known server row.
    ///
    /// This clears the local conflict marker only. It does not write a new row
    /// to the server and does not remove still-pending mutations for the same
    /// id.
    pub async fn use_server(self, id: Id) -> SyncResult<bool> {
        self.collection.clear_conflict(id.to_row_key()?).await
    }

    /// Retry a conflicted local edit against the latest canonical base version.
    ///
    /// This queues a new save. The existing conflict marker stays visible until
    /// the server accepts the retry and returns a new canonical row.
    pub async fn retry_local<Draft>(self, id: Id, draft: Draft) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        self.save(id, draft).await
    }

    /// Retry a conflicted local edit with explicit save options.
    pub async fn retry_local_with_options<Draft>(
        self,
        id: Id,
        draft: Draft,
        options: SaveOptions<Row>,
    ) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        self.save_with_options(id, draft, options).await
    }

    /// Discard every queued local mutation for one row and clear its conflict
    /// marker.
    ///
    /// This is the "throw away my local edits" resolution. The durable
    /// mutation queue purges first, then the in-memory pending overlay is
    /// dropped and the row falls back to its canonical (last server) value.
    /// Returns `true` if any in-memory state actually changed.
    pub async fn discard_local(self, id: Id) -> SyncResult<bool> {
        self.collection.discard_local(id.to_row_key()?).await
    }

    /// Submit an app-approved merge draft against the latest canonical base.
    pub async fn merge_with<Draft>(self, id: Id, draft: Draft) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        self.save(id, draft).await
    }

    /// Submit an app-approved merge draft with explicit save options.
    pub async fn merge_with_options<Draft>(
        self,
        id: Id,
        draft: Draft,
        options: SaveOptions<Row>,
    ) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        self.save_with_options(id, draft, options).await
    }

    async fn send<Draft>(
        self,
        id: Id,
        payload: CrudMutationPayload<Id, Draft>,
        optimistic: Option<SyncRow<Row>>,
        write_policy: WritePolicy,
        remove: bool,
    ) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        let draft = payload.into_sync_draft()?;
        self.send_draft(id, draft, optimistic, write_policy, remove)
            .await
    }

    async fn send_draft<Draft>(
        self,
        id: Id,
        draft: pocopine_sync::ClientMutationDraft<CrudMutationPayload<Id, Draft>>,
        optimistic: Option<SyncRow<Row>>,
        write_policy: WritePolicy,
        remove: bool,
    ) -> SyncResult<CrudOutcome<Id, Row>>
    where
        Draft: serde::Serialize + 'static,
    {
        match write_policy {
            WritePolicy::QueueOffline => {
                let mutation_id = self
                    .collection
                    .queue_with_generated_id(draft, optimistic)
                    .await?;
                Ok(CrudOutcome::Queued(crate::Queued::new(mutation_id, id)))
            }
            WritePolicy::RequireOnline => {
                let (mutation_id, response) = self
                    .collection
                    .push_with_generated_id_online_confirmed(draft, optimistic)
                    .await?;
                outcome_from_push_response(id, mutation_id, response, remove)
            }
        }
    }
}

fn outcome_from_push_response<Id, Row>(
    id: Id,
    mutation_id: MutationId,
    response: SyncPushResponse<Row>,
    remove: bool,
) -> SyncResult<CrudOutcome<Id, Row>>
where
    Id: ResourceId,
{
    if let Some(rejected) = response
        .rejected
        .into_iter()
        .find(|rejected| rejected.mutation_id == mutation_id)
    {
        return Ok(CrudOutcome::Rejected {
            id,
            reason: rejected.reason,
        });
    }

    if let Some(conflict) = response
        .conflicts
        .into_iter()
        .find(|conflict| conflict.mutation_id == mutation_id)
    {
        return Ok(CrudOutcome::Conflict {
            id,
            server_row: conflict.server_row.map(|row| row.value),
            reason: conflict.reason,
        });
    }

    if response
        .accepted
        .iter()
        .any(|accepted| accepted == &mutation_id)
    {
        if remove {
            return Ok(CrudOutcome::Removed { id });
        }

        let key = id.to_row_key()?;
        let row = response
            .rows
            .into_iter()
            .find(|row| row.key == key)
            .ok_or_else(|| {
                pocopine_sync::SyncError::client(format!(
                    "accepted CRUD mutation {mutation_id} did not include row {}",
                    key.as_str()
                ))
            })?;
        return Ok(CrudOutcome::Accepted { id, row: row.value });
    }

    Err(pocopine_sync::SyncError::client(format!(
        "push response did not include outcome for mutation {mutation_id}"
    )))
}

#[cfg(test)]
mod tests {
    #[cfg(not(target_arch = "wasm32"))]
    use std::{cell::RefCell, rc::Rc};

    #[cfg(not(target_arch = "wasm32"))]
    use pocopine_core::{Handle, ScopeId};
    #[cfg(not(target_arch = "wasm32"))]
    use pocopine_sync::{
        sync_plugin, CollectionState, LocalSnapshotBatch, MemoryLocalStore, SyncDeviceId,
        SyncLocalIdentity, SyncLocalStore, SyncStreamName,
    };
    use pocopine_sync::{
        MutationId, RowKey, RowVersion, SyncConflict, SyncPushResponse, SyncRejectedMutation,
        SyncRow,
    };
    use serde::{Deserialize, Serialize};

    use super::*;
    #[cfg(not(target_arch = "wasm32"))]
    use crate::local_resource_view;

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Post {
        title: String,
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[derive(Default)]
    struct TestState {
        posts: CollectionState<Post>,
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn posts(state: &mut TestState) -> &mut CollectionState<Post> {
        &mut state.posts
    }

    #[test]
    fn online_response_maps_accepted_remove_to_removed_outcome() {
        let mutation_id = MutationId::new("device_1:1").unwrap();
        let mut response =
            SyncPushResponse::new(pocopine_sync::SyncStreamName::new("posts").unwrap());
        response.accepted.push(mutation_id.clone());

        let outcome = outcome_from_push_response::<String, Post>(
            "post_1".to_string(),
            mutation_id,
            response,
            true,
        )
        .unwrap();

        assert_eq!(
            outcome,
            CrudOutcome::Removed {
                id: "post_1".to_string()
            }
        );
    }

    #[test]
    fn online_response_maps_accepted_row_to_accepted_outcome() {
        let mutation_id = MutationId::new("device_1:1").unwrap();
        let mut response =
            SyncPushResponse::new(pocopine_sync::SyncStreamName::new("posts").unwrap());
        response.accepted.push(mutation_id.clone());
        response.rows.push(
            SyncRow::new(
                "post_1",
                Post {
                    title: "server".to_string(),
                },
            )
            .unwrap(),
        );

        let outcome = outcome_from_push_response::<String, Post>(
            "post_1".to_string(),
            mutation_id,
            response,
            false,
        )
        .unwrap();

        assert_eq!(
            outcome,
            CrudOutcome::Accepted {
                id: "post_1".to_string(),
                row: Post {
                    title: "server".to_string()
                }
            }
        );
    }

    #[test]
    fn online_response_errors_when_accepted_row_is_absent() {
        let mutation_id = MutationId::new("device_1:1").unwrap();
        let mut response =
            SyncPushResponse::new(pocopine_sync::SyncStreamName::new("posts").unwrap());
        response.accepted.push(mutation_id.clone());

        let err = outcome_from_push_response::<String, Post>(
            "post_1".to_string(),
            mutation_id,
            response,
            false,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("accepted CRUD mutation device_1:1 did not include row post_1"));
    }

    #[test]
    fn online_response_maps_rejected_mutation() {
        let mutation_id = MutationId::new("device_1:1").unwrap();
        let mut response =
            SyncPushResponse::new(pocopine_sync::SyncStreamName::new("posts").unwrap());
        response.rejected.push(SyncRejectedMutation {
            mutation_id: mutation_id.clone(),
            key: None,
            reason: "invalid".to_string(),
        });

        let outcome = outcome_from_push_response::<String, Post>(
            "post_1".to_string(),
            mutation_id,
            response,
            false,
        )
        .unwrap();

        assert_eq!(
            outcome,
            CrudOutcome::Rejected {
                id: "post_1".to_string(),
                reason: "invalid".to_string()
            }
        );
    }

    #[test]
    fn online_response_maps_conflict_mutation() {
        let mutation_id = MutationId::new("device_1:1").unwrap();
        let mut response =
            SyncPushResponse::new(pocopine_sync::SyncStreamName::new("posts").unwrap());
        response.conflicts.push(SyncConflict {
            mutation_id: mutation_id.clone(),
            key: Some(RowKey::new("post_1").unwrap()),
            server_row: Some(
                SyncRow::new(
                    "post_1",
                    Post {
                        title: "server".to_string(),
                    },
                )
                .unwrap(),
            ),
            reason: "stale".to_string(),
        });

        let outcome = outcome_from_push_response::<String, Post>(
            "post_1".to_string(),
            mutation_id,
            response,
            false,
        )
        .unwrap();

        assert_eq!(
            outcome,
            CrudOutcome::Conflict {
                id: "post_1".to_string(),
                server_row: Some(Post {
                    title: "server".to_string()
                }),
                reason: "stale".to_string()
            }
        );
    }

    #[test]
    fn online_response_errors_when_mutation_has_no_outcome() {
        let mutation_id = MutationId::new("device_1:1").unwrap();
        let response = SyncPushResponse::new(pocopine_sync::SyncStreamName::new("posts").unwrap());

        let err = outcome_from_push_response::<String, Post>(
            "post_1".to_string(),
            mutation_id,
            response,
            false,
        )
        .unwrap_err();

        assert!(err
            .to_string()
            .contains("push response did not include outcome"));
    }

    #[test]
    fn save_options_can_use_view_base_version() {
        let view = LocalResourceView::<String, Post> {
            rows: vec![crate::LocalResourceRow {
                id: "post_1".to_string(),
                value: Post {
                    title: "server".to_string(),
                },
                row_version: Some(RowVersion::new("rendered").unwrap()),
                base_version: Some(RowVersion::new("canonical").unwrap()),
                status: crate::LocalResourceRowStatus::Synced,
            }],
            pending_mutations: Vec::new(),
            loading: false,
            syncing: false,
            stale: false,
            error: String::new(),
            version: 0,
            pending_count: 0,
            conflict_count: 0,
            rejected_count: 0,
            last_reason: pocopine_sync::SyncReason::Manual,
        };

        assert_eq!(
            view.base_version(&"post_1".to_string()).unwrap().as_str(),
            "canonical"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn use_server_clears_conflict_in_store_and_state() {
        let store = Rc::new(MemoryLocalStore::new());
        let stream = SyncStreamName::new("posts").unwrap();
        let state = Rc::new(RefCell::new(TestState::default()));
        let handle = Handle::new(state.clone(), ScopeId(1));
        let row = SyncRow::new(
            "post_1",
            Post {
                title: "server".to_string(),
            },
        )
        .unwrap()
        .conflict(true);

        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                pocopine_sync::SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", serde_json::json!({"title": "server"}))
                        .unwrap()
                        .conflict(true),
                ],
                None,
            ))
            .await
            .unwrap();
        state
            .borrow_mut()
            .posts
            .apply_local_snapshot(vec![row], None, 0);

        let sync = sync_plugin()
            .shared_local_store(store.clone())
            .into_client();
        let collection = sync
            .collection(handle, posts)
            .stream(stream.as_str())
            .unwrap();
        let view = local_resource_view::<String, _>(&state.borrow().posts).unwrap();
        let client = client_resource(collection, view);

        assert!(client.use_server("post_1".to_string()).await.unwrap());

        assert!(!state.borrow().posts.rows[0].conflict);
        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert!(!snapshot.rows[0].conflict);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn discard_local_drops_pending_and_clears_conflict() {
        let store = Rc::new(MemoryLocalStore::new());
        store
            .save_identity(SyncLocalIdentity::new(
                SyncDeviceId::new("device_discard_test").unwrap(),
            ))
            .await
            .unwrap();

        let stream = SyncStreamName::new("posts").unwrap();
        let state = Rc::new(RefCell::new(TestState::default()));
        let handle = Handle::new(state.clone(), ScopeId(3));

        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                pocopine_sync::SyncCollectionName::new("posts").unwrap(),
                vec![
                    SyncRow::new("post_1", serde_json::json!({"title": "server"}))
                        .unwrap()
                        .version("server_v1")
                        .unwrap()
                        .conflict(true),
                ],
                None,
            ))
            .await
            .unwrap();
        state.borrow_mut().posts.apply_local_snapshot(
            vec![SyncRow::new(
                "post_1",
                Post {
                    title: "server".to_string(),
                },
            )
            .unwrap()
            .version("server_v1")
            .unwrap()
            .conflict(true)],
            None,
            0,
        );

        let sync = sync_plugin()
            .shared_local_store(store.clone())
            .into_client();

        let collection = sync
            .clone()
            .collection(handle.clone(), posts)
            .stream(stream.as_str())
            .unwrap();
        let view = local_resource_view::<String, _>(&state.borrow().posts).unwrap();
        let client = client_resource(collection, view);
        let _ = client
            .retry_local(
                "post_1".to_string(),
                Post {
                    title: "local edit".to_string(),
                },
            )
            .await
            .unwrap();
        assert_eq!(store.pending_mutations(&stream).await.unwrap().len(), 1);

        let collection = sync
            .collection(handle, posts)
            .stream(stream.as_str())
            .unwrap();
        let view = local_resource_view::<String, _>(&state.borrow().posts).unwrap();
        let client = client_resource(collection, view);

        assert!(client.discard_local("post_1".to_string()).await.unwrap());

        assert!(store.pending_mutations(&stream).await.unwrap().is_empty());
        assert!(!state.borrow().posts.rows[0].conflict);
        assert!(!state.borrow().posts.rows[0].pending);
        assert_eq!(state.borrow().posts.rows[0].value.title, "server");
        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert!(!snapshot.rows[0].conflict);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn retry_local_queues_save_with_conflict_base_version() {
        let store = Rc::new(MemoryLocalStore::new());
        store
            .save_identity(SyncLocalIdentity::new(
                SyncDeviceId::new("device_retry_test").unwrap(),
            ))
            .await
            .unwrap();

        let stream = SyncStreamName::new("posts").unwrap();
        let state = Rc::new(RefCell::new(TestState::default()));
        let handle = Handle::new(state.clone(), ScopeId(2));
        state.borrow_mut().posts.apply_local_snapshot(
            vec![SyncRow::new(
                "post_1",
                Post {
                    title: "server".to_string(),
                },
            )
            .unwrap()
            .version("server_v2")
            .unwrap()
            .conflict(true)],
            None,
            0,
        );

        let sync = sync_plugin()
            .shared_local_store(store.clone())
            .into_client();
        let collection = sync
            .collection(handle, posts)
            .stream(stream.as_str())
            .unwrap();
        let view = local_resource_view::<String, _>(&state.borrow().posts).unwrap();
        let client = client_resource(collection, view);

        let outcome = client
            .retry_local(
                "post_1".to_string(),
                Post {
                    title: "local retry".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            outcome,
            CrudOutcome::Queued(crate::Queued::new(
                MutationId::new("device_retry_test:1").unwrap(),
                "post_1".to_string()
            ))
        );

        let pending = store.pending_mutations(&stream).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].key.as_ref().unwrap().as_str(), "post_1");
        assert_eq!(
            pending[0].base_version.as_ref().unwrap().as_str(),
            "server_v2"
        );
        assert_eq!(pending[0].payload["op"], "save");
        assert!(state.borrow().posts.rows[0].conflict);
    }
}
