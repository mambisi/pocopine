use std::{marker::PhantomData, rc::Rc};

use pocopine_core::{App, AppPlugin, Handle};
#[cfg(target_arch = "wasm32")]
use serde_json::Value;

use crate::{
    ClientMutation, CollectionState, MemoryLocalStore, SyncCursor, SyncError, SyncLocalStore,
    SyncReason, SyncResult, SyncRow, SyncStreamName, SYNC_ENDPOINT_PREFIX,
};

#[cfg(target_arch = "wasm32")]
use crate::{
    sync_stream_tag, LocalChangeBatch, LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot,
    SyncChange, SyncConflict, SyncOpenRequest, SyncOpenResponse, SyncPullMode, SyncPullRequest,
    SyncPullResponse, SyncPushRequest, SyncPushResponse,
};

/// Selector from an app-owned component/store into one sync collection field.
pub type CollectionSelector<C, T> = for<'a> fn(&'a mut C) -> &'a mut CollectionState<T>;

/// App plugin that provides [`SyncClient`] to components.
pub type SyncLocalStoreHandle = Rc<dyn SyncLocalStore>;

#[derive(Clone)]
pub struct SyncClientPlugin {
    endpoint: String,
    live_endpoint: Option<String>,
    live_wakeup: bool,
    with_credentials: bool,
    local_store: SyncLocalStoreHandle,
}

impl Default for SyncClientPlugin {
    fn default() -> Self {
        Self {
            endpoint: SYNC_ENDPOINT_PREFIX.to_string(),
            live_endpoint: None,
            live_wakeup: false,
            with_credentials: false,
            local_store: Rc::new(MemoryLocalStore::new()),
        }
    }
}

/// Build the sync app plugin.
///
/// The plugin is explicit: apps that do not install it do not get a
/// [`SyncClient`] service. Live wake-up is opt-in so apps can use sync
/// without also mounting `pocopine-live`.
pub fn sync_plugin() -> SyncClientPlugin {
    SyncClientPlugin::default()
}

impl SyncClientPlugin {
    /// Override the sync HTTP endpoint prefix.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Enable or disable live wake-up integration.
    pub fn with_live_wakeup(mut self, enabled: bool) -> Self {
        self.live_wakeup = enabled;
        self
    }

    /// Override the live stream endpoint when live wake-up is enabled.
    pub fn live_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.live_endpoint = Some(endpoint.into());
        self
    }

    /// Set browser credentials mode for sync fetches and live wake-ups.
    pub fn with_credentials(mut self, enabled: bool) -> Self {
        self.with_credentials = enabled;
        self
    }

    /// Use a custom local sync store.
    ///
    /// The default store is [`MemoryLocalStore`], which is useful for tests
    /// and demos but does not survive page reloads.
    pub fn local_store(mut self, store: impl SyncLocalStore + 'static) -> Self {
        self.local_store = Rc::new(store);
        self
    }

    /// Use a shared local sync store handle.
    pub fn shared_local_store(mut self, store: SyncLocalStoreHandle) -> Self {
        self.local_store = store;
        self
    }
}

impl AppPlugin for SyncClientPlugin {
    fn name(&self) -> &'static str {
        "pocopine-sync"
    }

    fn install(self, app: App) -> App {
        app.provide_plugin(SyncClient {
            endpoint: self.endpoint,
            live_endpoint: self.live_endpoint,
            live_wakeup: self.live_wakeup,
            with_credentials: self.with_credentials,
            local_store: self.local_store,
        })
    }
}

/// Runtime sync client service installed by [`sync_plugin`].
#[derive(Clone)]
pub struct SyncClient {
    endpoint: String,
    live_endpoint: Option<String>,
    live_wakeup: bool,
    with_credentials: bool,
    local_store: SyncLocalStoreHandle,
}

impl SyncClient {
    /// Build a collection runner bound to a component or store handle.
    pub fn collection<C, T>(
        &self,
        handle: Handle<C>,
        selector: CollectionSelector<C, T>,
    ) -> SyncCollection<C, T>
    where
        C: 'static,
        T: 'static,
    {
        SyncCollection {
            handle,
            selector,
            endpoint: self.endpoint.clone(),
            live_endpoint: self.live_endpoint.clone(),
            live_wakeup: self.live_wakeup,
            with_credentials: self.with_credentials,
            local_store: self.local_store.clone(),
            stream: None,
            cursor: None,
            _marker: PhantomData,
        }
    }
}

/// Scope-bound sync collection runner.
pub struct SyncCollection<C: 'static, T> {
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    endpoint: String,
    live_endpoint: Option<String>,
    live_wakeup: bool,
    with_credentials: bool,
    local_store: SyncLocalStoreHandle,
    stream: Option<SyncStreamName>,
    cursor: Option<SyncCursor>,
    _marker: PhantomData<fn(C) -> T>,
}

impl<C, T> SyncCollection<C, T>
where
    C: 'static,
    T: 'static,
{
    /// Set the server-registered stream to pull.
    pub fn stream(mut self, stream: impl Into<String>) -> SyncResult<Self> {
        self.stream = Some(SyncStreamName::new(stream.into())?);
        Ok(self)
    }

    /// Resume from a previously stored sync cursor.
    pub fn cursor(mut self, cursor: impl Into<String>) -> SyncResult<Self> {
        self.cursor = Some(SyncCursor::new(cursor.into())?);
        Ok(self)
    }

    /// Override live wake-up for this collection.
    pub fn with_live_wakeup(mut self, enabled: bool) -> Self {
        self.live_wakeup = enabled;
        self
    }

    /// Pull initial data and, when configured, open a live wake-up stream.
    pub fn open(self) -> SyncResult<()>
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        self.open_impl()
    }

    /// Trigger a manual pull.
    pub fn pull(self) -> SyncResult<()>
    where
        T: serde::de::DeserializeOwned + serde::Serialize,
    {
        self.pull_impl(SyncReason::Manual, false)
    }

    /// Push one client mutation and apply an optional optimistic row while
    /// the server confirms, rejects, or conflicts the write.
    pub fn push<M>(
        self,
        mutation: ClientMutation<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize,
        M: serde::Serialize + 'static,
    {
        self.push_impl(mutation, optimistic)
    }

    fn stream_value(&self) -> SyncResult<SyncStreamName> {
        self.stream
            .clone()
            .ok_or_else(|| SyncError::invalid_value("stream", "<missing>"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn touch_host_fields(&self) {
        let _ = (
            &self.handle,
            self.selector,
            &self.endpoint,
            &self.live_endpoint,
            self.live_wakeup,
            self.with_credentials,
            &self.local_store,
        );
    }
}

#[cfg(target_arch = "wasm32")]
impl<C, T> SyncCollection<C, T>
where
    C: 'static,
    T: serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    fn open_impl(self) -> SyncResult<()> {
        let stream = self.stream_value()?;
        let scope_id = pocopine_core::current_scope_id().ok_or_else(|| {
            SyncError::client(
                "SyncCollection::open used outside a component handler/lifecycle hook",
            )
        })?;

        let handle = self.handle.clone();
        let selector = self.selector;
        let live_wakeup = self.live_wakeup.then(|| LiveWakeupOptions {
            live_endpoint: self.live_endpoint.clone(),
            with_credentials: self.with_credentials,
        });
        start_open_then_pull(
            scope_id,
            handle.clone(),
            selector,
            self.endpoint.clone(),
            self.local_store.clone(),
            stream.clone(),
            self.cursor.clone(),
            SyncReason::Initial,
            live_wakeup,
        );
        Ok(())
    }

    fn pull_impl(self, reason: SyncReason, live_event: bool) -> SyncResult<()> {
        let stream = self.stream_value()?;
        let scope_id = pocopine_core::current_scope_id().ok_or_else(|| {
            SyncError::client(
                "SyncCollection::pull used outside a component handler/lifecycle hook",
            )
        })?;
        start_pull(
            scope_id,
            self.handle,
            self.selector,
            self.endpoint,
            self.local_store,
            stream,
            self.cursor,
            reason,
            live_event,
        );
        Ok(())
    }

    fn push_impl<M>(
        self,
        mutation: ClientMutation<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        let stream = self.stream_value()?;
        let scope_id = pocopine_core::current_scope_id().ok_or_else(|| {
            SyncError::client(
                "SyncCollection::push used outside a component handler/lifecycle hook",
            )
        })?;
        start_push(
            scope_id,
            self.handle,
            self.selector,
            self.endpoint,
            self.local_store,
            stream,
            mutation,
            optimistic,
            !self.live_wakeup,
        );
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<C, T> SyncCollection<C, T>
where
    C: 'static,
    T: 'static,
{
    fn open_impl(self) -> SyncResult<()> {
        self.touch_host_fields();
        let _ = self.stream_value()?;
        Ok(())
    }

    fn pull_impl(self, _reason: SyncReason, _live_event: bool) -> SyncResult<()> {
        self.touch_host_fields();
        let _ = self.stream_value()?;
        Ok(())
    }

    fn push_impl<M>(
        self,
        mutation: ClientMutation<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        self.touch_host_fields();
        let _ = self.stream_value()?;
        let _ = (mutation, optimistic);
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
struct LiveWakeupOptions {
    live_endpoint: Option<String>,
    with_credentials: bool,
}

#[cfg(target_arch = "wasm32")]
fn start_open_then_pull<C, T>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    cursor: Option<SyncCursor>,
    reason: SyncReason,
    live_wakeup: Option<LiveWakeupOptions>,
) where
    C: 'static,
    T: serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    let open_url = endpoint_path(&endpoint, "open");
    let pull_url = endpoint_path(&endpoint, "pull");
    let push_url = endpoint_path(&endpoint, "push");

    pocopine_core::spawn_for_scope(scope_id, async move {
        let mut local_cursor = None;
        let mut pending_mutations = Vec::new();
        let mut local_error = None;

        match local_store.hydrate_stream(&stream).await {
            Ok(snapshot) => {
                local_cursor = snapshot.cursor.clone();
                pending_mutations = snapshot.pending_mutations.clone();
                match decode_local_snapshot(snapshot) {
                    Ok((rows, cursor, pending_count)) => {
                        handle.update(|state| {
                            selector(state).apply_local_snapshot(rows, cursor, pending_count);
                        });
                    }
                    Err(err) => {
                        local_error = Some(format!("local sync cache decode failed: {err}"));
                    }
                }
            }
            Err(err) => {
                local_error = Some(format!("local sync cache hydrate failed: {err}"));
            }
        }

        let request_token = handle.update(|state| {
            let collection = selector(state);
            let cursor = cursor
                .or_else(|| collection.cursor.clone())
                .or(local_cursor);
            let token = if collection.version == 0 {
                collection.begin_initial()
            } else {
                collection.begin_pull(reason)
            };
            (cursor, token)
        });
        let (cursor, token) = request_token;

        let open_request = SyncOpenRequest::new([stream.clone()]);
        let open_result = pocopine_core::fetch::call::<SyncOpenRequest, SyncOpenResponse>(
            &open_url,
            &open_request,
        )
        .await;
        if let Err(err) = open_result.and_then(|response| validate_open_response(response, &stream))
        {
            handle.update(|state| {
                selector(state).apply_error(token, err);
            });
            return;
        }

        if let Some(live_wakeup) = live_wakeup {
            open_live_wakeup(
                scope_id,
                handle.clone(),
                selector,
                endpoint.clone(),
                local_store.clone(),
                stream.clone(),
                live_wakeup,
            );
        }

        if !pending_mutations.is_empty() {
            match replay_pending_mutations::<T>(
                &local_store,
                &push_url,
                stream.clone(),
                pending_mutations,
            )
            .await
            {
                Ok(()) => {}
                Err(err) => {
                    local_error = Some(format!("pending sync mutation replay failed: {err}"));
                }
            }
        }

        let request = SyncPullRequest::new(stream).cursor(cursor);
        let result =
            pocopine_core::fetch::call::<SyncPullRequest, SyncPullResponse<T>>(&pull_url, &request)
                .await;
        let result = match result {
            Ok(response) => {
                if let Err(err) = persist_pull_response(&local_store, &response).await {
                    local_error = Some(format!("local sync cache persist failed: {err}"));
                }
                Ok(response)
            }
            Err(err) => Err(err),
        };
        handle.update(|state| {
            let collection = selector(state);
            match result {
                Ok(response) => {
                    collection.apply_pull(token, response);
                    if let Some(error) = local_error {
                        collection.set_error(error);
                    }
                }
                Err(err) => {
                    collection.apply_error(token, err);
                }
            }
        });
    });
}

#[cfg(target_arch = "wasm32")]
fn open_live_wakeup<C, T>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    options: LiveWakeupOptions,
) where
    C: 'static,
    T: serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    let live_tag = sync_stream_tag(stream.as_str());
    let mut refresh = pocopine_live::LiveRefresh::scoped()
        .query_tag(live_tag, {
            let handle = handle.clone();
            let endpoint = endpoint.clone();
            let stream = stream.clone();
            move |event| {
                let reason = if matches!(event.live_event, pocopine_live::LiveEvent::Gap { .. }) {
                    SyncReason::Gap
                } else {
                    SyncReason::Live
                };
                start_pull(
                    scope_id,
                    handle.clone(),
                    selector,
                    endpoint.clone(),
                    local_store.clone(),
                    stream.clone(),
                    None,
                    reason,
                    true,
                );
            }
        })
        .on_error({
            let handle = handle.clone();
            move |event| {
                handle.update(|state| {
                    selector(state).set_error(format!("live wake-up failed: {:?}", event.target));
                });
            }
        })
        .with_credentials(options.with_credentials);

    if let Some(live_endpoint) = options.live_endpoint {
        refresh = refresh.endpoint(live_endpoint);
    }

    match refresh.open_unscoped() {
        Ok(subscription) => {
            pocopine_core::on_scope_unmount_for(scope_id, move || drop(subscription));
        }
        Err(err) => {
            handle.update(|state| {
                selector(state).set_error(format!("live wake-up failed: {err}"));
            });
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn validate_open_response(
    response: SyncOpenResponse,
    stream: &SyncStreamName,
) -> Result<(), pocopine_core::ServerError> {
    if response.protocol != crate::SYNC_PROTOCOL_V1 {
        return Err(pocopine_core::ServerError::BadRequest(format!(
            "unsupported sync protocol: {}",
            response.protocol
        )));
    }

    if response
        .streams
        .iter()
        .any(|accepted| accepted.stream == *stream)
    {
        Ok(())
    } else {
        Err(pocopine_core::ServerError::Forbidden(format!(
            "sync stream was not opened: {stream}"
        )))
    }
}

#[cfg(target_arch = "wasm32")]
fn start_pull<C, T>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    cursor: Option<SyncCursor>,
    reason: SyncReason,
    live_event: bool,
) where
    C: 'static,
    T: serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    let pull_url = endpoint_path(&endpoint, "pull");

    pocopine_core::spawn_for_scope(scope_id, async move {
        let request_token = handle.update(|state| {
            let collection = selector(state);
            let cursor = cursor.or_else(|| collection.cursor.clone());
            let request = SyncPullRequest::new(stream.clone()).cursor(cursor);
            let token = if live_event {
                collection.begin_live_pull(reason)
            } else if collection.version == 0 {
                collection.begin_initial()
            } else {
                collection.begin_pull(reason)
            };
            (request, token)
        });
        let (request, token) = request_token;
        let result =
            pocopine_core::fetch::call::<SyncPullRequest, SyncPullResponse<T>>(&pull_url, &request)
                .await;
        let mut local_error = None;
        let result = match result {
            Ok(response) => {
                if let Err(err) = persist_pull_response(&local_store, &response).await {
                    local_error = Some(format!("local sync cache persist failed: {err}"));
                }
                Ok(response)
            }
            Err(err) => Err(err),
        };
        handle.update(|state| {
            let collection = selector(state);
            match result {
                Ok(response) => {
                    collection.apply_pull(token, response);
                    if let Some(error) = local_error {
                        collection.set_error(error);
                    }
                }
                Err(err) => {
                    collection.apply_error(token, err);
                }
            }
        });
    });
}

#[cfg(target_arch = "wasm32")]
fn start_push<C, T, M>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    mutation: ClientMutation<M>,
    optimistic: Option<SyncRow<T>>,
    pull_after_accept: bool,
) where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
    M: serde::Serialize + 'static,
{
    let push_url = endpoint_path(&endpoint, "push");
    let pull_endpoint = endpoint.clone();
    let mutation_id = mutation.id.clone();
    let mutation_op = mutation.op;
    let mutation_key = mutation.key.clone();

    pocopine_core::spawn_for_scope(scope_id, async move {
        let local_mutation = match mutation_to_value(&mutation) {
            Ok(mutation) => mutation,
            Err(err) => {
                handle.update(|state| {
                    selector(state).set_error(format!("sync mutation encode failed: {err}"));
                });
                return;
            }
        };
        if let Err(err) = local_store.enqueue_mutation(&stream, local_mutation).await {
            handle.update(|state| {
                selector(state).set_error(format!("local sync mutation enqueue failed: {err}"));
            });
            return;
        }

        handle.update(|state| {
            selector(state).apply_optimistic_mutation(
                mutation_id,
                mutation_op,
                mutation_key,
                optimistic,
            );
        });

        let request = SyncPushRequest::new(stream.clone(), [mutation]);
        let result = pocopine_core::fetch::call::<SyncPushRequest<M>, SyncPushResponse<T>>(
            &push_url, &request,
        )
        .await;
        let mut local_error = None;
        let result = match result {
            Ok(response) => {
                match local_push_result_from_response(&response) {
                    Ok(result) => {
                        if let Err(err) = local_store.mark_push_result(result).await {
                            local_error =
                                Some(format!("local sync push result persist failed: {err}"));
                        }
                    }
                    Err(err) => {
                        local_error = Some(format!("local sync push result encode failed: {err}"));
                    }
                }
                Ok(response)
            }
            Err(err) => Err(err),
        };
        let should_pull = handle.update(|state| {
            let collection = selector(state);
            match result {
                Ok(response) => {
                    let should_pull = collection.apply_push(response);
                    if let Some(error) = local_error {
                        collection.set_error(error);
                    }
                    should_pull
                }
                Err(err) => {
                    collection.set_error(err.to_string());
                    false
                }
            }
        });

        if should_pull && pull_after_accept {
            start_pull(
                scope_id,
                handle,
                selector,
                pull_endpoint,
                local_store,
                stream,
                None,
                SyncReason::Push,
                false,
            );
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn decode_local_snapshot<T>(
    snapshot: LocalStreamSnapshot,
) -> SyncResult<(Vec<SyncRow<T>>, Option<SyncCursor>, u64)>
where
    T: serde::de::DeserializeOwned,
{
    let pending_count = snapshot.pending_mutations.len() as u64;
    let rows = snapshot
        .rows
        .into_iter()
        .map(row_from_value)
        .collect::<SyncResult<Vec<_>>>()?;
    Ok((rows, snapshot.cursor, pending_count))
}

#[cfg(target_arch = "wasm32")]
async fn replay_pending_mutations<T>(
    local_store: &SyncLocalStoreHandle,
    push_url: &str,
    stream: SyncStreamName,
    pending_mutations: Vec<ClientMutation<Value>>,
) -> SyncResult<()>
where
    T: serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    let request = SyncPushRequest::new(stream, pending_mutations);
    let response = pocopine_core::fetch::call::<SyncPushRequest<Value>, SyncPushResponse<T>>(
        push_url, &request,
    )
    .await
    .map_err(|err| SyncError::client(err.to_string()))?;
    let result = local_push_result_from_response(&response)?;
    local_store.mark_push_result(result).await
}

#[cfg(target_arch = "wasm32")]
async fn persist_pull_response<T>(
    local_store: &SyncLocalStoreHandle,
    response: &SyncPullResponse<T>,
) -> SyncResult<()>
where
    T: serde::Serialize,
{
    match response.mode {
        SyncPullMode::Snapshot => {
            let rows = response
                .rows
                .iter()
                .map(row_to_value)
                .collect::<SyncResult<Vec<_>>>()?;
            local_store
                .save_snapshot(LocalSnapshotBatch::new(
                    response.stream.clone(),
                    response.collection.clone(),
                    rows,
                    response.cursor.clone(),
                ))
                .await
        }
        SyncPullMode::Incremental => {
            let changes = response
                .changes
                .iter()
                .map(change_to_value)
                .collect::<SyncResult<Vec<_>>>()?;
            local_store
                .apply_changes(LocalChangeBatch::new(
                    response.stream.clone(),
                    response.collection.clone(),
                    changes,
                    response.cursor.clone(),
                ))
                .await
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn local_push_result_from_response<T>(response: &SyncPushResponse<T>) -> SyncResult<LocalPushResult>
where
    T: serde::Serialize,
{
    Ok(LocalPushResult {
        stream: response.stream.clone(),
        accepted: response.accepted.clone(),
        rejected: response.rejected.clone(),
        rows: response
            .rows
            .iter()
            .map(row_to_value)
            .collect::<SyncResult<Vec<_>>>()?,
        conflicts: response
            .conflicts
            .iter()
            .map(conflict_to_value)
            .collect::<SyncResult<Vec<_>>>()?,
        cursor: response.cursor.clone(),
    })
}

#[cfg(target_arch = "wasm32")]
fn mutation_to_value<M>(mutation: &ClientMutation<M>) -> SyncResult<ClientMutation<Value>>
where
    M: serde::Serialize,
{
    Ok(ClientMutation {
        id: mutation.id.clone(),
        key: mutation.key.clone(),
        op: mutation.op,
        base_version: mutation.base_version.clone(),
        payload: serde_json::to_value(&mutation.payload)?,
    })
}

#[cfg(target_arch = "wasm32")]
fn row_to_value<T>(row: &SyncRow<T>) -> SyncResult<SyncRow<Value>>
where
    T: serde::Serialize,
{
    Ok(SyncRow {
        key: row.key.clone(),
        version: row.version.clone(),
        value: serde_json::to_value(&row.value)?,
        pending: row.pending,
        conflict: row.conflict,
    })
}

#[cfg(target_arch = "wasm32")]
fn row_from_value<T>(row: SyncRow<Value>) -> SyncResult<SyncRow<T>>
where
    T: serde::de::DeserializeOwned,
{
    Ok(SyncRow {
        key: row.key,
        version: row.version,
        value: serde_json::from_value(row.value)?,
        pending: row.pending,
        conflict: row.conflict,
    })
}

#[cfg(target_arch = "wasm32")]
fn change_to_value<T>(change: &SyncChange<T>) -> SyncResult<SyncChange<Value>>
where
    T: serde::Serialize,
{
    Ok(SyncChange {
        stream: change.stream.clone(),
        collection: change.collection.clone(),
        key: change.key.clone(),
        op: change.op,
        row: change.row.as_ref().map(row_to_value).transpose()?,
        cursor: change.cursor.clone(),
    })
}

#[cfg(target_arch = "wasm32")]
fn conflict_to_value<T>(conflict: &SyncConflict<T>) -> SyncResult<SyncConflict<Value>>
where
    T: serde::Serialize,
{
    Ok(SyncConflict {
        mutation_id: conflict.mutation_id.clone(),
        key: conflict.key.clone(),
        server_row: conflict.server_row.as_ref().map(row_to_value).transpose()?,
        reason: conflict.reason.clone(),
    })
}

#[cfg(target_arch = "wasm32")]
fn endpoint_path(endpoint: &str, suffix: &str) -> String {
    format!("{}/{}", endpoint.trim_end_matches('/'), suffix)
}
