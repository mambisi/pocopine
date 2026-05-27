use std::{cell::Cell, marker::PhantomData, rc::Rc};

use pocopine_core::{App, AppPlugin, Handle};
use serde_json::Value;

use crate::{
    sign_out::{broadcast_sign_out, SignOutSubscription},
    ClientMutation, ClientMutationDraft, CollectionState, LocalPendingMutation, MemoryLocalStore,
    MutationId, RowKey, SyncCursor, SyncError, SyncLocalStore, SyncPushResponse, SyncReason,
    SyncResult, SyncRow, SyncStreamName, SYNC_ENDPOINT_PREFIX,
};

/// In-tab sign-out generation counter shared between [`SyncClient`] and every
/// [`SyncCollection`] it builds. `sign_out` bumps the inner cell before
/// touching the durable store, so any spawned pull/push task that completes
/// after the bump can observe staleness and drop its persist + state update
/// instead of repopulating the just-wiped cache.
///
/// On host targets the `started` snapshot and `is_stale` check are unused
/// because the host-side sync methods don't spawn background tasks — they
/// are sync stubs. The struct compiles on both targets so the field can be
/// stored uniformly on `SyncCollection` without `#[cfg]` plumbing.
#[derive(Clone)]
pub(crate) struct SyncEpoch {
    current: Rc<Cell<u64>>,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    started: u64,
}

impl SyncEpoch {
    fn new() -> Self {
        Self {
            current: Rc::new(Cell::new(0)),
            started: 0,
        }
    }

    /// Capture the current generation as the snapshot value for a new
    /// collection or spawned task.
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            current: self.current.clone(),
            started: self.current.get(),
        }
    }

    /// `true` when a sign-out has happened since this snapshot was captured.
    /// Spawned tasks check this before any post-fetch persist or state
    /// update. Reachable from host code only via tests; host runtime
    /// methods are sync stubs that don't spawn.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub(crate) fn is_stale(&self) -> bool {
        self.current.get() != self.started
    }

    /// Advance the shared generation. Called by `SyncClient::sign_out` before
    /// touching the durable store so a concurrent task that's about to check
    /// the gate sees the new value.
    fn bump(&self) {
        self.current.set(self.current.get().wrapping_add(1));
    }
}

#[cfg(target_arch = "wasm32")]
use crate::{
    sync_stream_tag, LocalChangeBatch, LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot,
    PendingMutation, SyncChange, SyncConflict, SyncOp, SyncOpenRequest, SyncOpenResponse,
    SyncPullMode, SyncPullRequest, SyncPullResponse, SyncPushRequest,
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
    epoch: SyncEpoch,
}

impl Default for SyncClientPlugin {
    fn default() -> Self {
        Self {
            endpoint: SYNC_ENDPOINT_PREFIX.to_string(),
            live_endpoint: None,
            live_wakeup: false,
            with_credentials: false,
            local_store: Rc::new(MemoryLocalStore::new()),
            epoch: SyncEpoch::new(),
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

    /// Build the runtime sync client from this plugin configuration.
    ///
    /// Most apps install the plugin on [`App`]; tests and custom runners can
    /// use this to bind generated resource helpers without mounting an app.
    pub fn into_client(self) -> SyncClient {
        SyncClient {
            endpoint: self.endpoint,
            live_endpoint: self.live_endpoint,
            live_wakeup: self.live_wakeup,
            with_credentials: self.with_credentials,
            local_store: self.local_store,
            epoch: self.epoch,
        }
    }
}

impl AppPlugin for SyncClientPlugin {
    fn name(&self) -> &'static str {
        "pocopine-sync"
    }

    fn install(self, app: App) -> App {
        app.provide_plugin(self.into_client())
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
    epoch: SyncEpoch,
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
            epoch: self.epoch.snapshot(),
            schema_version: crate::default_schema_version_one(),
            _marker: PhantomData,
        }
    }

    /// Durably drop the local sync cache and tell other tabs to do the same.
    ///
    /// This is the helper apps call from their auth-aware sign-out / tenant-
    /// switch path. The wipe happens first (the durability contract is
    /// [`SyncLocalStore::clear_all_streams`](crate::SyncLocalStore::clear_all_streams));
    /// the cross-tab broadcast goes out after, so any sibling tab that
    /// receives the event sees an already-empty store on the next read.
    ///
    /// The local store is not an authorization boundary — call this whenever
    /// the user identity, tenant, or auth scope changes. Mounted collections
    /// in this tab continue to hold their in-memory `CollectionState` until
    /// the app re-mounts them; sibling tabs registered with
    /// [`watch_sign_outs`](Self::watch_sign_outs) receive a notification so
    /// they can do the same.
    pub async fn sign_out(&self) -> SyncResult<()> {
        // Bump the epoch BEFORE awaiting the durable wipe. Any spawned
        // pull/push task that yields here and resumes later will compare
        // the new shared value against its captured snapshot and abort its
        // persist + state update — so a slow in-flight server response
        // cannot repopulate the cache after the sign-out wipe runs.
        //
        // **Failure semantics:** if `clear_all_streams` errors, the
        // epoch stays bumped and the broadcast never fires. Every
        // subsequent operation on the existing `SyncCollection`s in
        // this tab will return `SyncError::Unauthorized("sync session
        // ended")` even though the durable store may still hold
        // pre-sign-out data. We do NOT roll the bump back, because a
        // partial wipe would leave the store in an unknown state that
        // is more dangerous to keep using than the conservative
        // "session ended" posture. Apps observing the Err should
        // treat the session as permanently invalidated and require a
        // page reload (which re-mounts collections against a fresh
        // empty store, or against whatever durable state survived).
        //
        // **Error variant overload:** spawned tasks that race a
        // successful sign-out return `SyncError::unauthorized("sync
        // session ended")` to signal "the local session you queued
        // this against is gone." This shares the
        // `SyncError::Unauthorized` variant with credential-failure
        // errors from `CrudSource` impls; apps that want to
        // distinguish "show re-auth UI" from "navigate to sign-in
        // screen" should match on the message text or wrap this layer
        // with their own error mapping.
        self.epoch.bump();
        self.local_store.clear_all_streams().await?;
        broadcast_sign_out();
        Ok(())
    }

    /// Subscribe to cross-tab sign-out broadcasts.
    ///
    /// When another tab in the same browser origin calls
    /// [`sign_out`](Self::sign_out), the handler runs in this tab. Typical
    /// usage is to navigate back to a sign-in screen or drop mounted
    /// `CollectionState`s so the next render hydrates from the (now empty)
    /// local store.
    ///
    /// On host targets this is a no-op stub — there is no cross-process
    /// cascade today. The returned guard is still valid and dropping it is
    /// safe.
    #[cfg(target_arch = "wasm32")]
    pub fn watch_sign_outs(&self, handler: impl Fn() + 'static) -> SignOutSubscription {
        let inner = crate::sign_out::subscribe_sign_out(std::rc::Rc::new(handler));
        SignOutSubscription::wasm(inner)
    }

    /// Host-target stub. See the wasm32 doc comment.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn watch_sign_outs(&self, _handler: impl Fn() + 'static) -> SignOutSubscription {
        SignOutSubscription::noop()
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
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    epoch: SyncEpoch,
    /// Application-level schema version this collection's resource is
    /// encoded under. Generated `Resource::collection()` chains
    /// `.schema_version(SCHEMA_VERSION)` so push requests carry the
    /// resource's declared version. The server compares against its
    /// own `SyncStreamSource::schema_version()` and routes stale
    /// mutations through `migrate_payload`. Defaults to `1` for raw
    /// `SyncCollection` callers that don't go through the macro.
    schema_version: u32,
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

    /// Tag this collection's push requests with the resource's
    /// application-level schema version. Generated `Resource::collection()`
    /// calls this with the macro-emitted `SCHEMA_VERSION`; bare
    /// `SyncCollection` callers can leave the default (`1`).
    pub fn schema_version(mut self, version: u32) -> SyncResult<Self> {
        if version == 0 {
            return Err(SyncError::client(
                "schema_version must be >= 1 (schema versions start at 1)",
            ));
        }
        self.schema_version = version;
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
        T: Clone + serde::de::DeserializeOwned + serde::Serialize,
    {
        self.open_impl()
    }

    /// Trigger a manual pull.
    pub fn pull(self) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize,
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

    /// Push one client mutation without adding it to the durable offline queue.
    ///
    /// This is the low-level primitive for writes that must be confirmed by
    /// the server before the caller treats them as successful. The optional
    /// optimistic row is visible during the request, but it is rolled back if
    /// the request fails before the server returns an accepted, rejected, or
    /// conflict response.
    pub fn push_online<M>(
        self,
        mutation: ClientMutation<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize,
        M: serde::Serialize + 'static,
    {
        self.push_online_impl(mutation, optimistic)
    }

    /// Reserve a durable mutation id, push one draft mutation, and apply an
    /// optional optimistic row while the server confirms, rejects, or
    /// conflicts the write.
    pub fn push_with_generated_id<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize,
        M: serde::Serialize + 'static,
    {
        self.push_with_generated_id_impl(mutation, optimistic)
    }

    /// Reserve a durable mutation id and push one draft mutation without
    /// adding it to the durable offline queue.
    ///
    /// The mutation id counter is still persisted before the request starts, so
    /// a failed request may skip an id. Skipping is intentional: the id must not
    /// be reused after a browser crash or network failure.
    pub fn push_with_generated_id_online<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize,
        M: serde::Serialize + 'static,
    {
        self.push_with_generated_id_online_impl(mutation, optimistic)
    }

    /// Reserve a durable mutation id, enqueue the mutation locally, apply
    /// optimistic state, and return the reserved id after the local enqueue
    /// succeeds.
    ///
    /// This is the stronger local-first boundary used by generated CRUD
    /// resource helpers. The returned id is safe to expose because the local
    /// store has already persisted the incremented mutation counter and queued
    /// mutation before this future resolves.
    ///
    /// Mutation ids are monotonic, not dense: if id reservation succeeds but
    /// enqueueing fails, the reserved id is intentionally skipped instead of
    /// reused. On the browser runtime, a successful local enqueue immediately
    /// starts a background push; network errors are reflected in collection
    /// state, not in this returned queued outcome. The host implementation is a
    /// compile/test stub and only reserves and enqueues locally.
    pub async fn queue_with_generated_id<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<MutationId>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        self.queue_with_generated_id_impl(mutation, optimistic)
            .await
    }

    /// Reserve a durable mutation id and push one generated-id mutation without
    /// adding it to the durable offline queue, waiting for the server response.
    ///
    /// This is the async primitive for `WritePolicy::RequireOnline`. The host
    /// implementation returns `SyncError::Unsupported`; confirmed browser
    /// pushes require the wasm fetch runtime. Mutation ids are still
    /// monotonic, not dense: a failed confirmed push may skip an id after the
    /// counter has been durably reserved. On request errors, collection state
    /// reflects the push error for the optimistic mutation before this future
    /// returns `Err`.
    pub async fn push_with_generated_id_online_confirmed<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<(MutationId, SyncPushResponse<T>)>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        self.push_with_generated_id_online_confirmed_impl(mutation, optimistic)
            .await
    }

    /// Clear a local conflict marker for one row after the user resolves it.
    ///
    /// This persists the local metadata change before updating mounted
    /// collection state. It does not write server data or clear unrelated
    /// pending mutations, so stale pending writes for the same row may still
    /// conflict again when replayed.
    pub async fn clear_conflict(self, key: RowKey) -> SyncResult<bool>
    where
        T: Clone,
    {
        let stream = self.stream_value()?;
        let local_store = self.local_store.clone();
        let handle = self.handle;
        let selector = self.selector;
        local_store.clear_conflict(&stream, &key).await?;
        clear_conflict_in_handle(handle, selector, &key)
    }

    /// Discard every pending mutation for one row, clear its conflict marker,
    /// and rebase the rendered view from canonical.
    ///
    /// This is the local-edit equivalent of `clear_conflict`: queued local
    /// writes for the row are dropped from the durable mutation queue and
    /// from in-memory state. Unkeyed pending mutations (e.g. a stream-wide
    /// `Reset`) and mutations for other rows are left intact. The durable
    /// purge happens before the in-memory rebase so a mid-flight crash
    /// cannot leave the in-memory view ahead of disk.
    pub async fn discard_local(self, key: RowKey) -> SyncResult<bool>
    where
        T: Clone,
    {
        let stream = self.stream_value()?;
        let local_store = self.local_store.clone();
        let handle = self.handle;
        let selector = self.selector;
        local_store.purge_pending_for_row(&stream, &key).await?;
        local_store.clear_conflict(&stream, &key).await?;
        discard_local_in_handle(handle, selector, &key)
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
fn clear_conflict_in_handle<C, T>(
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    key: &RowKey,
) -> SyncResult<bool>
where
    C: 'static,
    T: Clone,
{
    Ok(handle.update(|state| selector(state).clear_conflict(key)))
}

#[cfg(not(target_arch = "wasm32"))]
fn clear_conflict_in_handle<C, T>(
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    key: &RowKey,
) -> SyncResult<bool>
where
    C: 'static,
    T: Clone,
{
    // Host sync collection methods are test stubs, so use the borrowable
    // handle directly. The browser path keeps `Handle::update` reactivity.
    let mut state = handle.try_borrow_mut().map_err(|_| {
        SyncError::client("sync collection state is already borrowed while clearing conflict")
    })?;
    Ok(selector(&mut state).clear_conflict(key))
}

#[cfg(target_arch = "wasm32")]
fn discard_local_in_handle<C, T>(
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    key: &RowKey,
) -> SyncResult<bool>
where
    C: 'static,
    T: Clone,
{
    Ok(handle.update(|state| selector(state).discard_local(key)))
}

#[cfg(not(target_arch = "wasm32"))]
fn discard_local_in_handle<C, T>(
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    key: &RowKey,
) -> SyncResult<bool>
where
    C: 'static,
    T: Clone,
{
    let mut state = handle.try_borrow_mut().map_err(|_| {
        SyncError::client("sync collection state is already borrowed while discarding local edits")
    })?;
    Ok(selector(&mut state).discard_local(key))
}

#[cfg(target_arch = "wasm32")]
impl<C, T> SyncCollection<C, T>
where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
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
            self.epoch.clone(),
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
            self.epoch,
            // Explicit refresh path: no fresh /open, so we don't have
            // an advertised version to stamp. The UPSERT's coalesce
            // preserves whatever `start_open_then_pull` already wrote.
            None,
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
        let schema_version = self.schema_version;
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
            true,
            self.epoch,
            schema_version,
        );
        Ok(())
    }

    fn push_online_impl<M>(
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
                "SyncCollection::push_online used outside a component handler/lifecycle hook",
            )
        })?;
        let schema_version = self.schema_version;
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
            false,
            self.epoch,
            schema_version,
        );
        Ok(())
    }

    fn push_with_generated_id_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        let stream = self.stream_value()?;
        let scope_id = pocopine_core::current_scope_id().ok_or_else(|| {
            SyncError::client(
                "SyncCollection::push_with_generated_id used outside a component handler/lifecycle hook",
            )
        })?;
        let schema_version = self.schema_version;
        start_push_with_generated_id(
            scope_id,
            self.handle,
            self.selector,
            self.endpoint,
            self.local_store,
            stream,
            mutation,
            optimistic,
            !self.live_wakeup,
            true,
            self.epoch,
            schema_version,
        );
        Ok(())
    }

    fn push_with_generated_id_online_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<()>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        let stream = self.stream_value()?;
        let scope_id = pocopine_core::current_scope_id().ok_or_else(|| {
            SyncError::client(
                "SyncCollection::push_with_generated_id_online used outside a component handler/lifecycle hook",
            )
        })?;
        let schema_version = self.schema_version;
        start_push_with_generated_id(
            scope_id,
            self.handle,
            self.selector,
            self.endpoint,
            self.local_store,
            stream,
            mutation,
            optimistic,
            !self.live_wakeup,
            false,
            self.epoch,
            schema_version,
        );
        Ok(())
    }

    async fn queue_with_generated_id_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<MutationId>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        let stream = self.stream_value()?;
        let scope_id = self.handle.scope_id();
        let push_url = endpoint_path(&self.endpoint, "push");
        let pull_endpoint = self.endpoint.clone();
        let pull_after_accept = !self.live_wakeup;
        // Gate each user-initiated `.await` so a concurrent sign-out
        // (e.g. triggered by a BroadcastChannel listener that fires
        // between yields) cannot leave a stale pending mutation in the
        // just-wiped store. The post-await checks consult the epoch
        // BEFORE the `?` propagates any inner error — a backend error
        // that fired because the wipe rolled the transaction back
        // should surface as `Unauthorized` ("sync session ended"), not
        // as a generic `Backend` error the app can't recognize.
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        let reserve_result = self.local_store.reserve_mutation_id().await;
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        let mutation_id = reserve_result?;
        let mutation = mutation.with_id(mutation_id.clone());
        let enqueue_result =
            enqueue_pending_mutation(&self.local_store, &stream, &mutation, optimistic.as_ref())
                .await;
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        enqueue_result?;
        apply_optimistic_mutation(&self.handle, self.selector, &mutation, optimistic);

        let epoch = self.epoch.clone();
        let schema_version = self.schema_version;
        pocopine_core::spawn_for_scope(scope_id, async move {
            let _ = send_push_and_reconcile(
                scope_id,
                self.handle,
                self.selector,
                push_url,
                pull_endpoint,
                self.local_store,
                stream,
                mutation,
                pull_after_accept,
                true,
                epoch,
                schema_version,
            )
            .await;
        });

        Ok(mutation_id)
    }

    async fn push_with_generated_id_online_confirmed_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<(MutationId, SyncPushResponse<T>)>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        let stream = self.stream_value()?;
        let scope_id = self.handle.scope_id();
        let push_url = endpoint_path(&self.endpoint, "push");
        let pull_endpoint = self.endpoint.clone();
        let pull_after_accept = !self.live_wakeup;
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        let reserve_result = self.local_store.reserve_mutation_id().await;
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        let mutation_id = reserve_result?;
        let mutation = mutation.with_id(mutation_id.clone());
        apply_optimistic_mutation(&self.handle, self.selector, &mutation, optimistic);
        let schema_version = self.schema_version;
        let response = send_push_and_reconcile(
            scope_id,
            self.handle,
            self.selector,
            push_url,
            pull_endpoint,
            self.local_store,
            stream,
            mutation,
            pull_after_accept,
            false,
            self.epoch,
            schema_version,
        )
        .await?;
        Ok((mutation_id, response))
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

    fn push_online_impl<M>(
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

    fn push_with_generated_id_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
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

    fn push_with_generated_id_online_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
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

    async fn queue_with_generated_id_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<MutationId>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        self.touch_host_fields();
        let stream = self.stream_value()?;
        // Each post-await stale check consults the epoch BEFORE the `?`
        // propagates any inner error so a backend error caused by the
        // wipe surfaces as `Unauthorized` rather than `Backend` —
        // matching the wasm path's contract.
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        let reserve_result = self.local_store.reserve_mutation_id().await;
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        let mutation_id = reserve_result?;
        let mutation = mutation.with_id(mutation_id.clone());
        let enqueue_result =
            enqueue_pending_mutation(&self.local_store, &stream, &mutation, optimistic.as_ref())
                .await;
        // Third stale check mirrors the wasm path — even though the host
        // stub does no in-memory apply, returning Ok here would lie about
        // the queue state when sign-out raced the enqueue await.
        if self.epoch.is_stale() {
            return Err(SyncError::unauthorized("sync session ended"));
        }
        enqueue_result?;
        // Host-side collection calls are no-op stubs; the browser path applies
        // the optimistic row before starting the background push.
        let _ = optimistic;
        Ok(mutation_id)
    }

    async fn push_with_generated_id_online_confirmed_impl<M>(
        self,
        mutation: ClientMutationDraft<M>,
        optimistic: Option<SyncRow<T>>,
    ) -> SyncResult<(MutationId, SyncPushResponse<T>)>
    where
        T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
        M: serde::Serialize + 'static,
    {
        self.touch_host_fields();
        let _ = self.stream_value()?;
        let _ = (mutation, optimistic);
        Err(SyncError::unsupported(
            "online sync push confirmation is only available in the browser runtime",
        ))
    }
}

#[cfg(target_arch = "wasm32")]
struct LiveWakeupOptions {
    live_endpoint: Option<String>,
    with_credentials: bool,
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
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
    epoch: SyncEpoch,
) where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    let open_url = endpoint_path(&endpoint, "open");
    let pull_url = endpoint_path(&endpoint, "pull");
    let push_url = endpoint_path(&endpoint, "push");

    pocopine_core::spawn_for_scope(scope_id, async move {
        let mut local_cursor = None;
        let mut pending_mutations = Vec::new();
        let mut local_error = None;
        let mut cached_schema_version: Option<u32> = None;

        let hydrate_result = local_store.hydrate_stream(&stream).await;
        // Sign-out can fire while we're awaiting hydrate; if so, drop
        // the result on the floor — the wipe is in progress and our
        // pre-wipe snapshot would write tenant A's rows into the
        // orphaned collection.
        if epoch.is_stale() {
            return;
        }
        match hydrate_result {
            Ok(snapshot) => {
                local_cursor = snapshot.cursor.clone();
                cached_schema_version = snapshot.application_schema_version;
                pending_mutations = snapshot
                    .pending_mutations
                    .iter()
                    .map(|pending| pending.mutation.clone())
                    .collect();
                match decode_local_snapshot(snapshot) {
                    Ok(decoded) => {
                        handle.update(|state| {
                            selector(state).apply_local_snapshot_with_pending(
                                decoded.rows,
                                decoded.cursor,
                                decoded.pending_mutations,
                            );
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
        // Symmetric with the pull/push error paths below: if sign-out
        // raced the /open fetch, neither the error state nor the live
        // subscription belong on this collection any more.
        if epoch.is_stale() {
            return;
        }
        let advertised_schema_version =
            match open_result.and_then(|response| validate_open_response(response, &stream)) {
                Ok(v) => v,
                Err(err) => {
                    handle.update(|state| {
                        selector(state).apply_error(token, err);
                    });
                    return;
                }
            };

        // Decide whether the local cache (rows + pending mutations)
        // is still valid for the server's currently-advertised schema
        // version:
        //
        // * `cached < advertised` — server upgraded; the cache and
        //   queue are stale shape. Drop them durably + in memory and
        //   rebuild via the upcoming pull.
        // * `cached > advertised` — server ROLLED BACK; the local
        //   cache is "newer than the server" but locally consistent.
        //   Do NOT wipe; hold the queue AND skip replay/persist
        //   entirely so a transient older-shard read doesn't
        //   downgrade the durable fence or replay v2 mutations
        //   tagged as v1.
        // * `cached == advertised` — match; no work needed.
        // * `cached == None` + any durable state (cursor, cached
        //   rows, pending mutations, OR an in-memory decode error
        //   from hydrate) — store has never observed an advertised
        //   version (fresh install OR pre-v4 row whose
        //   `app_schema_version` column is NULL) AND has non-empty
        //   state of unknown shape. Safest action is to drop it all
        //   (lossy but bounded; fires once on the upgrade boundary).
        // * `cached == None` + truly empty — fresh install; adopt
        //   the advertised value silently on the next snapshot save.
        let mut cursor = cursor;
        let mut token = token;
        let rolled_back = matches!(
            cached_schema_version,
            Some(cached) if cached > advertised_schema_version
        );
        // NOTE: `local_cursor` was moved into the `request_token`
        // closure above; consult `cursor` (the materialized version
        // for this open) and `pending_mutations` to detect durable
        // state that survived a `cached_schema_version == None`
        // hydrate.
        let cached_none_has_durable_state = cached_schema_version.is_none()
            && (cursor.is_some() || !pending_mutations.is_empty() || local_error.is_some());
        let must_wipe = match cached_schema_version {
            Some(cached) if cached < advertised_schema_version => true,
            None if cached_none_has_durable_state => true,
            _ => false,
        };
        if must_wipe {
            tracing::info!(
                target: "pocopine.log",
                stream = stream.as_str(),
                cached_schema_version = ?cached_schema_version,
                advertised_schema_version,
                pending_mutations = pending_mutations.len(),
                "sync stream schema_version drift detected; clearing local cache + pending mutations",
            );
            if let Err(err) = local_store.clear_stream(&stream).await {
                // Fail loud: if the durable wipe failed, do NOT
                // proceed to pull + persist. A successful pull
                // would stamp the NEW app_schema_version via the
                // UPSERT coalesce, silently masking the wipe
                // failure forever. Surfacing the error keeps
                // cached at the OLD value so the next open
                // re-detects the mismatch and retries.
                handle.update(|state| {
                    selector(state).apply_error(
                        token,
                        format!("local sync cache wipe-on-schema-bump failed: {err}"),
                    );
                });
                return;
            }
            if epoch.is_stale() {
                return;
            }
            // In-memory side: hard reset via
            // `reset_for_schema_invalidation`. Unlike
            // `apply_local_snapshot_with_pending(empty, None, empty)`
            // (which short-circuits in `apply_local_snapshot_inner`
            // because all inputs are empty), this unconditionally
            // drops canonical_rows, rows, pending_mutations, cursor,
            // and bumps request_generation — so any in-flight token
            // becomes a no-op via `is_current`.
            pending_mutations.clear();
            let new_token = handle.update(|state| {
                let collection = selector(state);
                collection.reset_for_schema_invalidation();
                // Re-issue a token; the prior `request_token` was
                // invalidated by the reset's generation bump.
                collection.begin_initial()
            });
            token = new_token;
            // Force a Snapshot pull by nulling the cursor — the
            // OLD-schema cursor was wiped durably; sending it to
            // the server would yield an Incremental response that
            // merges new-schema deltas onto an empty canonical
            // set, leaving most rows missing until the next pull.
            cursor = None;
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
                epoch.clone(),
                advertised_schema_version,
            );
        }

        // Rolled-back servers: skip replay AND the upcoming pull's
        // durable persist. The client holds its queue + cache as-is;
        // the in-memory state is still derived from the existing
        // snapshot. A pull may still run to refresh the in-memory
        // view, but its response must NOT downgrade the durable
        // `app_schema_version` fence.
        if !pending_mutations.is_empty() && !rolled_back {
            if epoch.is_stale() {
                return;
            }
            match replay_pending_mutations::<T>(
                &local_store,
                &push_url,
                stream.clone(),
                pending_mutations,
                &epoch,
                advertised_schema_version,
            )
            .await
            {
                Ok(response) => {
                    if epoch.is_stale() {
                        return;
                    }
                    handle.update(|state| {
                        selector(state).apply_push(response);
                    });
                }
                Err(err) => {
                    local_error = Some(format!("pending sync mutation replay failed: {err}"));
                }
            }
        } else if rolled_back {
            tracing::info!(
                target: "pocopine.log",
                stream = stream.as_str(),
                cached_schema_version = ?cached_schema_version,
                advertised_schema_version,
                pending_mutations = pending_mutations.len(),
                "sync stream server appears to have rolled back; holding queue + cache"
            );
        }

        let request = SyncPullRequest::new(stream.clone()).cursor(cursor);
        let result =
            pocopine_core::fetch::call::<SyncPullRequest, SyncPullResponse<T>>(&pull_url, &request)
                .await;
        if epoch.is_stale() {
            return;
        }
        let result = match result {
            Ok(response) => {
                // Persist-time invalidation guard: if the in-memory
                // generation moved on (e.g. a concurrent
                // `start_open_then_pull` detected a schema mismatch
                // and durably wiped the stream while our pull was
                // in-flight), skip the persist so we don't
                // resurrect wiped state. The in-memory apply at the
                // bottom of this fn is already token-guarded.
                let still_current = handle.update(|state| selector(state).is_current(token));
                if still_current {
                    // Persist with the cached version under rollback so
                    // the durable fence stays at the higher value. When
                    // server catches back up, the next open will detect
                    // cached==advertised and resume normal flow.
                    let persist_version = if rolled_back {
                        cached_schema_version
                    } else {
                        Some(advertised_schema_version)
                    };
                    if let Err(err) =
                        persist_pull_response(&local_store, &response, persist_version).await
                    {
                        local_error = Some(format!("local sync cache persist failed: {err}"));
                    }
                } else {
                    tracing::info!(
                        target: "pocopine.log",
                        stream = stream.as_str(),
                        "sync stream pull response discarded: in-flight wipe invalidated this request"
                    );
                }
                Ok(response)
            }
            Err(err) => Err(err),
        };
        if epoch.is_stale() {
            return;
        }
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
#[allow(clippy::too_many_arguments)]
fn open_live_wakeup<C, T>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    options: LiveWakeupOptions,
    epoch: SyncEpoch,
    advertised_schema_version: u32,
) where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    let live_tag = sync_stream_tag(stream.as_str());
    let mut refresh = pocopine_live::LiveRefresh::scoped()
        .query_tag(live_tag, {
            let handle = handle.clone();
            let endpoint = endpoint.clone();
            let stream = stream.clone();
            let epoch = epoch.clone();
            move |event| {
                if epoch.is_stale() {
                    return;
                }
                let reason = if matches!(event.live_event, pocopine_live::LiveEvent::Gap { .. }) {
                    SyncReason::Gap
                } else {
                    SyncReason::Live
                };
                // Pass the captured-at-open advertised schema_version
                // through so the live-pull's `persist_pull_response`
                // stamps the durable `app_schema_version` if the
                // post-/open pull hasn't done so yet. Without this,
                // a live event firing before `start_open_then_pull`'s
                // persist completes would write rows + cursor without
                // the schema_version, leaving the durable column NULL
                // — and the next session would skip the mismatch
                // check because cached == None.
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
                    epoch.clone(),
                    Some(advertised_schema_version),
                );
            }
        })
        .on_error({
            let handle = handle.clone();
            let epoch = epoch.clone();
            move |event| {
                if epoch.is_stale() {
                    return;
                }
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
            if epoch.is_stale() {
                return;
            }
            handle.update(|state| {
                selector(state).set_error(format!("live wake-up failed: {err}"));
            });
        }
    }
}

// The function body is pure (no fetch / no DOM); we compile it on
// host targets too so the test module below can exercise the new
// domain checks. Production callers live in the wasm-only
// `start_open_then_pull`, so on host outside tests this is dead-code
// and warned-as-error by `-D warnings`; the cfg below mirrors the
// trybuild pattern of "wasm prod OR host test".
#[cfg(any(target_arch = "wasm32", test))]
fn validate_open_response(
    response: crate::SyncOpenResponse,
    stream: &SyncStreamName,
) -> Result<u32, pocopine_core::ServerError> {
    if response.protocol != crate::SYNC_PROTOCOL_V1 {
        return Err(pocopine_core::ServerError::BadRequest(format!(
            "unsupported sync protocol: {}",
            response.protocol
        )));
    }

    // Reject a server that lists the same stream twice. The match is
    // load-bearing for cache invalidation in Batch 2 — picking the
    // first arbitrarily on duplicates would let a buggy server pin the
    // client to a stale schema_version.
    let matches: Vec<&crate::SyncOpenStream> = response
        .streams
        .iter()
        .filter(|accepted| accepted.stream == *stream)
        .collect();
    let accepted = match matches.as_slice() {
        [] => {
            return Err(pocopine_core::ServerError::Forbidden(format!(
                "sync stream was not opened: {stream}"
            )));
        }
        [one] => *one,
        _ => {
            return Err(pocopine_core::ServerError::BadRequest(format!(
                "server returned duplicate open entries for stream: {stream}"
            )));
        }
    };
    // Enforce the same `>= 1` invariant the macro + builder enforce on
    // the producing side. A buggy/malicious server advertising 0 would
    // otherwise collide with the planned `Option<u32>::None ==
    // 'never observed'` sentinel in Batch 2's cache-version compare.
    if accepted.schema_version == 0 {
        return Err(pocopine_core::ServerError::BadRequest(format!(
            "server advertised schema_version=0 for stream {stream}: versions start at 1"
        )));
    }
    Ok(accepted.schema_version)
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
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
    epoch: SyncEpoch,
    application_schema_version: Option<u32>,
) where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
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
        if epoch.is_stale() {
            return;
        }
        let mut local_error = None;
        let result = match result {
            Ok(response) => {
                // Persist-time invalidation guard: if the in-memory
                // generation moved on (concurrent wipe by
                // `start_open_then_pull`), skip the persist so we
                // don't resurrect wiped state. The in-memory apply
                // below is already token-guarded.
                let still_current = handle.update(|state| selector(state).is_current(token));
                if still_current {
                    // Callers that ran AFTER a successful /open pass
                    // `Some(advertised)` so the durable
                    // `app_schema_version` gets stamped even if the
                    // post-/open pull never runs. Callers without an
                    // advertised version (explicit refresh without
                    // re-opening) pass None and rely on the UPSERT's
                    // coalesce to preserve whatever value
                    // `start_open_then_pull` already persisted.
                    if let Err(err) =
                        persist_pull_response(&local_store, &response, application_schema_version)
                            .await
                    {
                        local_error = Some(format!("local sync cache persist failed: {err}"));
                    }
                } else {
                    tracing::info!(
                        target: "pocopine.log",
                        stream = stream.as_str(),
                        "sync stream pull response discarded: in-flight wipe invalidated this request"
                    );
                }
                Ok(response)
            }
            Err(err) => Err(err),
        };
        if epoch.is_stale() {
            return;
        }
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
#[allow(clippy::too_many_arguments)]
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
    queue_offline: bool,
    epoch: SyncEpoch,
    schema_version: u32,
) where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
    M: serde::Serialize + 'static,
{
    let push_url = endpoint_path(&endpoint, "push");
    let pull_endpoint = endpoint.clone();

    pocopine_core::spawn_for_scope(scope_id, async move {
        run_push(
            scope_id,
            handle,
            selector,
            push_url,
            pull_endpoint,
            local_store,
            stream,
            mutation,
            optimistic,
            pull_after_accept,
            queue_offline,
            epoch,
            schema_version,
        )
        .await;
    });
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
fn start_push_with_generated_id<C, T, M>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    mutation: ClientMutationDraft<M>,
    optimistic: Option<SyncRow<T>>,
    pull_after_accept: bool,
    queue_offline: bool,
    epoch: SyncEpoch,
    schema_version: u32,
) where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
    M: serde::Serialize + 'static,
{
    let push_url = endpoint_path(&endpoint, "push");
    let pull_endpoint = endpoint.clone();

    pocopine_core::spawn_for_scope(scope_id, async move {
        let mutation_id = match local_store.reserve_mutation_id().await {
            Ok(id) => id,
            Err(err) => {
                if epoch.is_stale() {
                    return;
                }
                handle.update(|state| {
                    selector(state).set_error(format!("sync mutation id allocation failed: {err}"));
                });
                return;
            }
        };
        if epoch.is_stale() {
            return;
        }
        run_push(
            scope_id,
            handle,
            selector,
            push_url,
            pull_endpoint,
            local_store,
            stream,
            mutation.with_id(mutation_id),
            optimistic,
            pull_after_accept,
            queue_offline,
            epoch,
            schema_version,
        )
        .await;
    });
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
async fn run_push<C, T, M>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    push_url: String,
    pull_endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    mutation: ClientMutation<M>,
    optimistic: Option<SyncRow<T>>,
    pull_after_accept: bool,
    queue_offline: bool,
    epoch: SyncEpoch,
    schema_version: u32,
) where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
    M: serde::Serialize + 'static,
{
    let mutation_id = mutation.id.clone();
    let mutation_op = mutation.op;
    let mutation_key = mutation.key.clone();

    if queue_offline {
        // Pre-await: if the task was spawned before sign-out and only
        // polled afterward, drop it before touching the store at all.
        if epoch.is_stale() {
            return;
        }
        let enqueue_result =
            enqueue_pending_mutation(&local_store, &stream, &mutation, optimistic.as_ref()).await;
        // Post-await: the durable write may have landed during the await
        // window between the bump and `clear_all_streams` completing —
        // that row will be cleared by the wipe. The post-await check
        // ensures we never compound the durable transient by also
        // writing the in-memory optimistic state for an orphaned tenant.
        if epoch.is_stale() {
            return;
        }
        if let Err(err) = enqueue_result {
            handle.update(|state| {
                selector(state).set_error(err.to_string());
            });
            return;
        }
    }

    if epoch.is_stale() {
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

    let _ = send_push_and_reconcile(
        scope_id,
        handle,
        selector,
        push_url,
        pull_endpoint,
        local_store,
        stream,
        mutation,
        pull_after_accept,
        queue_offline,
        epoch,
        schema_version,
    )
    .await;
}

async fn enqueue_pending_mutation<T, M>(
    local_store: &SyncLocalStoreHandle,
    stream: &SyncStreamName,
    mutation: &ClientMutation<M>,
    optimistic: Option<&SyncRow<T>>,
) -> SyncResult<()>
where
    M: serde::Serialize,
    T: serde::Serialize,
{
    let local_pending = pending_mutation_to_value(mutation, optimistic)
        .map_err(|err| SyncError::client(format!("sync mutation encode failed: {err}")))?;
    local_store
        .enqueue_pending_mutation(stream, local_pending)
        .await
        .map_err(|err| SyncError::client(format!("local sync mutation enqueue failed: {err}")))
}

#[cfg(target_arch = "wasm32")]
fn apply_optimistic_mutation<C, T, M>(
    handle: &Handle<C>,
    selector: CollectionSelector<C, T>,
    mutation: &ClientMutation<M>,
    optimistic: Option<SyncRow<T>>,
) where
    C: 'static,
    T: Clone + 'static,
{
    let mutation_id = mutation.id.clone();
    let mutation_op = mutation.op;
    let mutation_key = mutation.key.clone();
    handle.update(|state| {
        selector(state).apply_optimistic_mutation(
            mutation_id,
            mutation_op,
            mutation_key,
            optimistic,
        );
    });
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
async fn send_push_and_reconcile<C, T, M>(
    scope_id: pocopine_core::ScopeId,
    handle: Handle<C>,
    selector: CollectionSelector<C, T>,
    push_url: String,
    pull_endpoint: String,
    local_store: SyncLocalStoreHandle,
    stream: SyncStreamName,
    mutation: ClientMutation<M>,
    pull_after_accept: bool,
    reconcile_queued_mutation: bool,
    epoch: SyncEpoch,
    schema_version: u32,
) -> SyncResult<SyncPushResponse<T>>
where
    C: 'static,
    T: Clone + serde::de::DeserializeOwned + serde::Serialize + 'static,
    M: serde::Serialize + 'static,
{
    let mutation_id = mutation.id.clone();

    // `reconcile_queued_mutation` means the mutation is already in the
    // durable local queue. A server response should therefore be persisted as
    // the local push result so the queue can clear accepted/rejected/conflict
    // outcomes. Online-only pushes skip this local queue reconciliation.
    // `schema_version` tags the wire envelope so the server's
    // `push_handler` can route stale-schema mutations through
    // `migrate_payload` before delegating to the source's `push`.
    let request =
        SyncPushRequest::new(stream.clone(), [mutation]).with_schema_version(schema_version);
    let result =
        pocopine_core::fetch::call::<SyncPushRequest<M>, SyncPushResponse<T>>(&push_url, &request)
            .await;
    if epoch.is_stale() {
        return result.map_err(|err| SyncError::client(err.to_string()));
    }
    let mut local_error = None;
    let result: Result<SyncPushResponse<T>, String> = match result {
        Ok(response) => {
            if reconcile_queued_mutation {
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
            }
            Ok(response)
        }
        Err(err) => Err(err.to_string()),
    };
    let return_result = match &result {
        Ok(response) => Ok(response.clone()),
        Err(err) => Err(SyncError::client(err.clone())),
    };
    if epoch.is_stale() {
        return return_result;
    }
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
                if reconcile_queued_mutation {
                    collection.set_error(err.to_string());
                } else {
                    collection.apply_push_error(&mutation_id, err.to_string());
                }
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
            epoch,
            // Post-push pull: no advertised version known here; rely
            // on UPSERT coalesce to preserve the existing value.
            None,
        );
    }

    return_result
}

#[cfg(target_arch = "wasm32")]
struct DecodedLocalSnapshot<T> {
    rows: Vec<SyncRow<T>>,
    cursor: Option<SyncCursor>,
    pending_mutations: Vec<PendingMutation<T>>,
}

#[cfg(target_arch = "wasm32")]
fn decode_local_snapshot<T>(snapshot: LocalStreamSnapshot) -> SyncResult<DecodedLocalSnapshot<T>>
where
    T: serde::de::DeserializeOwned,
{
    let pending_mutations = snapshot
        .pending_mutations
        .into_iter()
        .map(pending_mutation_from_local)
        .collect::<SyncResult<Vec<_>>>()?;
    let rows = snapshot
        .rows
        .into_iter()
        .map(row_from_value)
        .collect::<SyncResult<Vec<_>>>()?;
    Ok(DecodedLocalSnapshot {
        rows,
        cursor: snapshot.cursor,
        pending_mutations,
    })
}

#[cfg(target_arch = "wasm32")]
async fn replay_pending_mutations<T>(
    local_store: &SyncLocalStoreHandle,
    push_url: &str,
    stream: SyncStreamName,
    pending_mutations: Vec<ClientMutation<Value>>,
    epoch: &SyncEpoch,
    schema_version: u32,
) -> SyncResult<SyncPushResponse<T>>
where
    T: serde::de::DeserializeOwned + serde::Serialize + 'static,
{
    let request =
        SyncPushRequest::new(stream, pending_mutations).with_schema_version(schema_version);
    let response = pocopine_core::fetch::call::<SyncPushRequest<Value>, SyncPushResponse<T>>(
        push_url, &request,
    )
    .await
    .map_err(|err| SyncError::client(err.to_string()))?;
    // Gate the durable `mark_push_result` write — otherwise a sign-out
    // between the /push and the durable persist could write tenant A's
    // accepted/rejected outcomes into the now-wiped store. Surface this
    // as an explicit `Unauthorized` error so the caller cannot
    // accidentally treat a stale-skipped replay as a successful one and
    // apply the server response to the (orphaned) in-memory collection
    // state.
    if epoch.is_stale() {
        return Err(SyncError::unauthorized("sync session ended"));
    }
    let result = local_push_result_from_response(&response)?;
    local_store.mark_push_result(result).await?;
    Ok(response)
}

#[cfg(target_arch = "wasm32")]
fn pending_mutation_from_local<T>(pending: LocalPendingMutation) -> SyncResult<PendingMutation<T>>
where
    T: serde::de::DeserializeOwned,
{
    let LocalPendingMutation {
        mutation,
        optimistic_row,
    } = pending;
    let optimistic = match optimistic_row {
        Some(row) => Some(row_from_value(row)?),
        None => pending_optimistic_from_payload(&mutation),
    };

    Ok(PendingMutation {
        id: mutation.id,
        op: mutation.op,
        key: mutation.key,
        before: None,
        before_rows: Vec::new(),
        optimistic,
    })
}

#[cfg(target_arch = "wasm32")]
fn pending_optimistic_from_payload<T>(mutation: &ClientMutation<Value>) -> Option<SyncRow<T>>
where
    T: serde::de::DeserializeOwned,
{
    match mutation.op {
        SyncOp::Upsert => mutation.key.clone().and_then(|key| {
            serde_json::from_value(mutation.payload.clone())
                .ok()
                .map(|value| SyncRow {
                    key,
                    version: mutation.base_version.clone(),
                    value,
                    pending: true,
                    conflict: false,
                })
        }),
        SyncOp::Delete | SyncOp::Reset => None,
    }
}

#[cfg(target_arch = "wasm32")]
async fn persist_pull_response<T>(
    local_store: &SyncLocalStoreHandle,
    response: &SyncPullResponse<T>,
    application_schema_version: Option<u32>,
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
                .save_snapshot(
                    LocalSnapshotBatch::new(
                        response.stream.clone(),
                        response.collection.clone(),
                        rows,
                        response.cursor.clone(),
                    )
                    .with_application_schema_version(application_schema_version),
                )
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
        collection: response.collection.clone(),
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
        migration_outcome: None,
    })
}

fn pending_mutation_to_value<M, T>(
    mutation: &ClientMutation<M>,
    optimistic: Option<&SyncRow<T>>,
) -> SyncResult<LocalPendingMutation>
where
    M: serde::Serialize,
    T: serde::Serialize,
{
    Ok(LocalPendingMutation::new(mutation_to_value(mutation)?)
        .with_optimistic_row(optimistic.map(row_to_value).transpose()?))
}

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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::{cell::RefCell, marker::PhantomData, rc::Rc};

    use pocopine_core::{Handle, ScopeId};
    use serde::{Deserialize, Serialize};

    use super::*;
    use crate::{
        ClientMutationDraft, CollectionState, LocalSnapshotBatch, MemoryLocalStore, RowKey,
        SyncCollectionName, SyncDeviceId, SyncLocalIdentity, SyncOp, SyncOpenResponse,
        SyncOpenStream, SyncRow,
    };

    #[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
    struct Post {
        title: String,
    }

    #[derive(Default)]
    struct TestState {
        posts: CollectionState<Post>,
    }

    fn posts(state: &mut TestState) -> &mut CollectionState<Post> {
        &mut state.posts
    }

    fn test_collection(
        store: SyncLocalStoreHandle,
        stream: SyncStreamName,
    ) -> (Rc<RefCell<TestState>>, SyncCollection<TestState, Post>) {
        let state = Rc::new(RefCell::new(TestState::default()));
        let handle = Handle::new(state.clone(), ScopeId(1));
        let collection = SyncCollection {
            handle,
            selector: posts,
            endpoint: SYNC_ENDPOINT_PREFIX.to_string(),
            live_endpoint: None,
            live_wakeup: false,
            with_credentials: false,
            local_store: store,
            stream: Some(stream),
            cursor: None,
            epoch: SyncEpoch::new(),
            schema_version: 1,
            _marker: PhantomData,
        };
        (state, collection)
    }

    #[tokio::test]
    async fn queue_with_generated_id_reserves_and_enqueues_before_returning() {
        let store: SyncLocalStoreHandle = Rc::new(MemoryLocalStore::new());
        store
            .save_identity(SyncLocalIdentity::new(
                SyncDeviceId::new("device_local").unwrap(),
            ))
            .await
            .unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let (state, collection) = test_collection(store.clone(), stream.clone());
        let row = SyncRow::new(
            "post_1",
            Post {
                title: "queued".to_string(),
            },
        )
        .unwrap();
        let draft =
            ClientMutationDraft::new(SyncOp::Upsert, row.value.clone()).row_key(row.key.clone());

        let mutation_id = collection
            .queue_with_generated_id(draft, Some(row.clone()))
            .await
            .unwrap();

        assert_eq!(mutation_id.as_str(), "device_local:1");
        let pending = store.pending_mutations(&stream).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, mutation_id);
        assert_eq!(pending[0].key.as_ref().unwrap().as_str(), "post_1");
        assert!(state.borrow().posts.rows.is_empty());
    }

    #[tokio::test]
    async fn generated_id_online_confirmed_is_unsupported_on_host() {
        let store: SyncLocalStoreHandle = Rc::new(MemoryLocalStore::new());
        let stream = SyncStreamName::new("posts").unwrap();
        let (_state, collection) = test_collection(store.clone(), stream.clone());
        let draft = ClientMutationDraft::new(
            SyncOp::Upsert,
            Post {
                title: "online".to_string(),
            },
        )
        .row_key(RowKey::new("post_1").unwrap());

        let err = collection
            .push_with_generated_id_online_confirmed(draft, None)
            .await
            .unwrap_err();

        assert!(matches!(err, SyncError::Unsupported(_)));
        assert!(store.pending_mutations(&stream).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn clear_conflict_updates_store_and_state_on_host() {
        let store: SyncLocalStoreHandle = Rc::new(MemoryLocalStore::new());
        let stream = SyncStreamName::new("posts").unwrap();
        let (state, collection) = test_collection(store.clone(), stream.clone());
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
                SyncCollectionName::new("posts").unwrap(),
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

        let changed = collection
            .clear_conflict(RowKey::new("post_1").unwrap())
            .await
            .unwrap();

        assert!(changed);
        assert!(!state.borrow().posts.rows[0].conflict);
        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert!(!snapshot.rows[0].conflict);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn sign_out_wipes_durable_store_and_preserves_identity() {
        let store = Rc::new(MemoryLocalStore::new());
        let stream = SyncStreamName::new("posts").unwrap();
        store
            .save_identity(
                SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_local").unwrap(), 7)
                    .unwrap(),
            )
            .await
            .unwrap();
        store
            .save_snapshot(LocalSnapshotBatch::new(
                stream.clone(),
                SyncCollectionName::new("posts").unwrap(),
                vec![SyncRow::new("post_1", serde_json::json!({"title": "server"})).unwrap()],
                None,
            ))
            .await
            .unwrap();

        let client = sync_plugin()
            .shared_local_store(store.clone())
            .into_client();
        client.sign_out().await.unwrap();

        let snapshot = store.hydrate_stream(&stream).await.unwrap();
        assert!(snapshot.rows.is_empty());
        let identity = store.load_identity().await.unwrap().unwrap();
        assert_eq!(identity.device_id.as_str(), "device_local");
        assert_eq!(identity.next_mutation_counter, 7);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn sign_out_bumps_epoch_so_collections_built_before_become_stale() {
        let store = Rc::new(MemoryLocalStore::new());
        let client = sync_plugin().shared_local_store(store).into_client();
        let state = Rc::new(RefCell::new(TestState::default()));
        let handle = Handle::new(state, ScopeId(42));
        // Build a collection BEFORE sign_out — captures the current epoch.
        let collection = client.collection(handle, posts);
        // Snapshot of the collection's captured epoch — must look fresh.
        assert!(!collection.epoch.is_stale());
        // sign_out bumps the shared cell. The collection's captured
        // snapshot is now older than current → is_stale() flips true.
        client.sign_out().await.unwrap();
        assert!(collection.epoch.is_stale());
        // A NEW collection built after sign_out captures the new epoch
        // and is fresh again — only the in-flight pre-signout collection
        // sees staleness.
        let state2 = Rc::new(RefCell::new(TestState::default()));
        let handle2 = Handle::new(state2, ScopeId(43));
        let fresh = client.collection(handle2, posts);
        assert!(!fresh.epoch.is_stale());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[tokio::test]
    async fn watch_sign_outs_returns_a_valid_guard_on_host() {
        let store = Rc::new(MemoryLocalStore::new());
        let client = sync_plugin().shared_local_store(store).into_client();
        // Host stub: should not panic, returns a guard, dropping is fine.
        let guard = client.watch_sign_outs(|| {});
        drop(guard);
    }

    #[test]
    fn validate_open_response_rejects_zero_schema_version() {
        // Wire defense — symmetric with macro + builder which both
        // reject 0. A buggy server advertising 0 would otherwise pin
        // the client to an invalid version that collides with future
        // sentinel encodings.
        let stream = SyncStreamName::new("posts").unwrap();
        let mut response = SyncOpenResponse::new(vec![SyncOpenStream {
            stream: stream.clone(),
            collection: SyncCollectionName::new("posts").unwrap(),
            cursor: None,
            schema_version: 0,
            params: ::std::collections::BTreeMap::new(),
        }]);
        // Sanity: protocol field is already set by `new`.
        response.protocol = crate::SYNC_PROTOCOL_V1.to_string();
        let err = validate_open_response(response, &stream).unwrap_err();
        match err {
            pocopine_core::ServerError::BadRequest(msg) => {
                assert!(msg.contains("schema_version=0"), "unexpected error: {msg}");
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[test]
    fn validate_open_response_rejects_duplicate_stream_entries() {
        // Wire defense — a duplicate stream entry would make the
        // version-pick ambiguous; reject rather than silently pick the
        // first.
        let stream = SyncStreamName::new("posts").unwrap();
        let response = SyncOpenResponse::new(vec![
            SyncOpenStream {
                stream: stream.clone(),
                collection: SyncCollectionName::new("posts").unwrap(),
                cursor: None,
                schema_version: 2,
                params: ::std::collections::BTreeMap::new(),
            },
            SyncOpenStream {
                stream: stream.clone(),
                collection: SyncCollectionName::new("posts").unwrap(),
                cursor: None,
                schema_version: 1,
                params: ::std::collections::BTreeMap::new(),
            },
        ]);
        let err = validate_open_response(response, &stream).unwrap_err();
        match err {
            pocopine_core::ServerError::BadRequest(msg) => {
                assert!(msg.contains("duplicate"), "unexpected error: {msg}");
            }
            other => panic!("expected BadRequest, got: {other:?}"),
        }
    }

    #[test]
    fn validate_open_response_accepts_advertised_schema_version() {
        let stream = SyncStreamName::new("posts").unwrap();
        let response = SyncOpenResponse::new(vec![SyncOpenStream {
            stream: stream.clone(),
            collection: SyncCollectionName::new("posts").unwrap(),
            cursor: None,
            schema_version: 3,
            params: ::std::collections::BTreeMap::new(),
        }]);
        let v = validate_open_response(response, &stream).unwrap();
        assert_eq!(v, 3);
    }
}

#[cfg(target_arch = "wasm32")]
fn endpoint_path(endpoint: &str, suffix: &str) -> String {
    format!("{}/{}", endpoint.trim_end_matches('/'), suffix)
}
