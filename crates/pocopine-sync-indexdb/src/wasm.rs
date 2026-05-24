use std::{collections::BTreeMap, fmt, rc::Rc};

use futures::lock::Mutex as AsyncMutex;
use js_sys::{Function, Promise, Reflect};
use pocopine_sync::{
    generate_sync_device_id, ClientMutation, LocalChangeBatch, LocalPendingMutation,
    LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot, MutationId, RowKey, SyncError,
    SyncLocalFuture, SyncLocalIdentity, SyncLocalStore, SyncOp, SyncResult, SyncRow,
    SyncStreamName,
};
use wasm_bindgen::{closure::Closure, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Event, IdbDatabase, IdbObjectStore, IdbOpenDbRequest, IdbRequest, IdbTransaction,
    IdbTransactionMode,
};

const DEFAULT_DATABASE_NAME: &str = "pocopine_sync";
const DATABASE_VERSION: u32 = 1;
const META_STORE: &str = "meta";
const STREAMS_STORE: &str = "streams";
const IDENTITY_KEY: &str = "identity";

thread_local! {
    // Conservative correctness gate: IndexedDB transaction lifetimes
    // are easy to get wrong across browsers, so the first shipped
    // backend serializes all local-store work. A future optimization
    // can narrow this to per-database or per-store gates once the
    // multi-stream paths have browser coverage.
    static IDB_GATE: Rc<AsyncMutex<()>> = Rc::new(AsyncMutex::new(()));
}

/// IndexedDB-backed [`SyncLocalStore`] for browser targets.
///
/// Unlike the OPFS SQLite backend, this store does not require
/// cross-origin isolation headers, so it works with third-party auth flows
/// that need hosted iframes or popups.
#[derive(Clone)]
pub struct IndexedDbLocalStore {
    database_name: String,
}

impl IndexedDbLocalStore {
    /// Open the default browser IndexedDB database.
    pub fn new() -> Self {
        Self {
            database_name: DEFAULT_DATABASE_NAME.to_string(),
        }
    }

    /// Open a named browser IndexedDB database.
    pub fn with_database_name(database_name: impl Into<String>) -> SyncResult<Self> {
        let database_name = validate_database_name(database_name.into())?;
        Ok(Self { database_name })
    }

    /// The IndexedDB database name used by this store.
    pub fn database_name(&self) -> &str {
        &self.database_name
    }

    fn run<T: 'static>(
        &self,
        task: impl std::future::Future<Output = SyncResult<T>> + 'static,
    ) -> SyncLocalFuture<'static, T> {
        Box::pin(async move {
            let gate = idb_gate();
            let _guard = gate.lock().await;
            task.await
        })
    }
}

impl Default for IndexedDbLocalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for IndexedDbLocalStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IndexedDbLocalStore")
            .field("database_name", &self.database_name)
            .finish()
    }
}

impl SyncLocalStore for IndexedDbLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>> {
        let database_name = self.database_name.clone();
        self.run(load_identity(database_name))
    }

    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()> {
        let database_name = self.database_name.clone();
        self.run(save_identity(database_name, identity))
    }

    fn reserve_mutation_id(&self) -> SyncLocalFuture<'_, MutationId> {
        let database_name = self.database_name.clone();
        self.run(reserve_mutation_id(database_name))
    }

    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot> {
        let database_name = self.database_name.clone();
        let stream = stream.clone();
        self.run(hydrate_stream(database_name, stream))
    }

    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()> {
        let database_name = self.database_name.clone();
        self.run(save_snapshot(database_name, snapshot))
    }

    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()> {
        let database_name = self.database_name.clone();
        self.run(apply_changes(database_name, changes))
    }

    fn enqueue_mutation(
        &self,
        stream: &SyncStreamName,
        mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()> {
        self.enqueue_pending_mutation(stream, LocalPendingMutation::new(mutation))
    }

    fn enqueue_pending_mutation(
        &self,
        stream: &SyncStreamName,
        pending: LocalPendingMutation,
    ) -> SyncLocalFuture<'_, ()> {
        let database_name = self.database_name.clone();
        let stream = stream.clone();
        self.run(enqueue_mutation(database_name, stream, pending))
    }

    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        let database_name = self.database_name.clone();
        self.run(mark_push_result(database_name, result))
    }

    fn clear_conflict(&self, stream: &SyncStreamName, key: &RowKey) -> SyncLocalFuture<'_, ()> {
        let database_name = self.database_name.clone();
        let stream = stream.clone();
        let key = key.clone();
        self.run(clear_conflict(database_name, stream, key))
    }

    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        let database_name = self.database_name.clone();
        let stream = stream.clone();
        self.run(async move {
            Ok(hydrate_stream(database_name, stream)
                .await?
                .pending_mutations
                .into_iter()
                .map(|pending| pending.mutation)
                .collect())
        })
    }
}

async fn load_identity(database_name: String) -> SyncResult<Option<SyncLocalIdentity>> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, META_STORE, IdbTransactionMode::Readonly)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(META_STORE).map_err(js_error)?;
    let value = get_string(&store, IDENTITY_KEY).await?;
    await_transaction(done).await?;
    database.close();
    value
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(Into::into)
}

async fn save_identity(database_name: String, identity: SyncLocalIdentity) -> SyncResult<()> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, META_STORE, IdbTransactionMode::Readwrite)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(META_STORE).map_err(js_error)?;
    put_string(&store, IDENTITY_KEY, &serde_json::to_string(&identity)?).await?;
    await_transaction(done).await?;
    database.close();
    Ok(())
}

async fn reserve_mutation_id(database_name: String) -> SyncResult<MutationId> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, META_STORE, IdbTransactionMode::Readwrite)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(META_STORE).map_err(js_error)?;
    let identity = get_string(&store, IDENTITY_KEY)
        .await?
        .map(|value| serde_json::from_str(&value))
        .transpose()?
        .unwrap_or(SyncLocalIdentity::new(generate_sync_device_id()?));
    let (id, advanced) = identity.reserve_mutation_id()?;
    put_string(&store, IDENTITY_KEY, &serde_json::to_string(&advanced)?).await?;
    await_transaction(done).await?;
    database.close();
    Ok(id)
}

async fn hydrate_stream(
    database_name: String,
    stream: SyncStreamName,
) -> SyncResult<LocalStreamSnapshot> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, STREAMS_STORE, IdbTransactionMode::Readonly)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(STREAMS_STORE).map_err(js_error)?;
    let snapshot = load_stream_state(&store, stream).await?;
    await_transaction(done).await?;
    database.close();
    Ok(snapshot)
}

async fn save_snapshot(database_name: String, snapshot: LocalSnapshotBatch) -> SyncResult<()> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, STREAMS_STORE, IdbTransactionMode::Readwrite)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(STREAMS_STORE).map_err(js_error)?;
    let mut state = load_stream_state(&store, snapshot.stream.clone()).await?;
    state.collection = Some(snapshot.collection);
    state.cursor = snapshot.cursor;
    state.rows = sorted_rows(snapshot.rows);
    put_stream_state(&store, &state).await?;
    await_transaction(done).await?;
    database.close();
    Ok(())
}

async fn apply_changes(database_name: String, changes: LocalChangeBatch) -> SyncResult<()> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, STREAMS_STORE, IdbTransactionMode::Readwrite)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(STREAMS_STORE).map_err(js_error)?;
    let mut state = load_stream_state(&store, changes.stream.clone()).await?;
    state.collection = Some(changes.collection);
    state.cursor = changes.cursor;
    let mut rows = rows_by_key(state.rows);
    let changes = changes_after_last_reset(changes.changes);
    if changes.had_reset {
        rows.clear();
    }

    for change in changes.items {
        match change.op {
            SyncOp::Upsert | SyncOp::Reset => {
                if let Some(row) = change.row {
                    rows.insert(row.key.clone(), row);
                }
            }
            SyncOp::Delete => {
                if let Some(key) = change.key {
                    rows.remove(&key);
                }
            }
        }
    }

    state.rows = rows.into_values().collect();
    put_stream_state(&store, &state).await?;
    await_transaction(done).await?;
    database.close();
    Ok(())
}

async fn enqueue_mutation(
    database_name: String,
    stream: SyncStreamName,
    pending: LocalPendingMutation,
) -> SyncResult<()> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, STREAMS_STORE, IdbTransactionMode::Readwrite)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(STREAMS_STORE).map_err(js_error)?;
    let mut state = load_stream_state(&store, stream).await?;
    if let Some(existing) = state
        .pending_mutations
        .iter_mut()
        .find(|existing| existing.mutation.id == pending.mutation.id)
    {
        *existing = pending;
    } else {
        state.pending_mutations.push(pending);
    }
    put_stream_state(&store, &state).await?;
    await_transaction(done).await?;
    database.close();
    Ok(())
}

async fn mark_push_result(database_name: String, result: LocalPushResult) -> SyncResult<()> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, STREAMS_STORE, IdbTransactionMode::Readwrite)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(STREAMS_STORE).map_err(js_error)?;
    let mut state = load_stream_state(&store, result.stream.clone()).await?;
    if let Some(collection) = result.collection {
        state.collection = Some(collection);
    }
    if let Some(cursor) = result.cursor {
        state.cursor = Some(cursor);
    }

    for id in result.accepted {
        state
            .pending_mutations
            .retain(|pending| pending.mutation.id != id);
    }

    for rejected in result.rejected {
        state
            .pending_mutations
            .retain(|pending| pending.mutation.id != rejected.mutation_id);
    }

    let mut rows = rows_by_key(state.rows);
    for conflict in result.conflicts {
        state
            .pending_mutations
            .retain(|pending| pending.mutation.id != conflict.mutation_id);
        if let Some(mut row) = conflict.server_row {
            row.pending = false;
            row.conflict = true;
            rows.insert(row.key.clone(), row);
        } else if let Some(key) = conflict.key {
            if let Some(row) = rows.get_mut(&key) {
                row.pending = false;
                row.conflict = true;
            }
        }
    }

    for mut row in result.rows {
        row.pending = false;
        row.conflict = false;
        rows.insert(row.key.clone(), row);
    }

    state.rows = rows.into_values().collect();
    put_stream_state(&store, &state).await?;
    await_transaction(done).await?;
    database.close();
    Ok(())
}

async fn clear_conflict(
    database_name: String,
    stream: SyncStreamName,
    key: RowKey,
) -> SyncResult<()> {
    let database = open_database(&database_name).await?;
    let transaction = transaction(&database, STREAMS_STORE, IdbTransactionMode::Readwrite)?;
    let done = transaction_done(&transaction);
    let store = transaction.object_store(STREAMS_STORE).map_err(js_error)?;
    let mut state = load_stream_state(&store, stream).await?;
    if let Some(row) = state.rows.iter_mut().find(|row| row.key == key) {
        row.conflict = false;
        put_stream_state(&store, &state).await?;
    }
    await_transaction(done).await?;
    database.close();
    Ok(())
}

async fn open_database(database_name: &str) -> SyncResult<IdbDatabase> {
    let indexed_db = web_sys::window()
        .ok_or_else(|| SyncError::client("window is not available"))?
        .indexed_db()
        .map_err(js_error)?
        .ok_or_else(|| SyncError::client("IndexedDB is not available"))?;
    let request = indexed_db
        .open_with_u32(database_name, DATABASE_VERSION)
        .map_err(js_error)?;
    install_upgrade_handler(&request);
    let request: IdbRequest = request.unchecked_into();
    request_value(request)
        .await
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| SyncError::client("IndexedDB open request returned a non-database value"))
}

fn install_upgrade_handler(request: &IdbOpenDbRequest) {
    let handler_request = request.clone();
    let on_upgrade = Closure::<dyn FnMut(Event)>::new(move |_event| {
        if let Ok(database) = handler_request
            .result()
            .and_then(|value| value.dyn_into::<IdbDatabase>())
        {
            let _ = database.create_object_store(META_STORE);
            let _ = database.create_object_store(STREAMS_STORE);
        }
    });
    request.set_onupgradeneeded(Some(on_upgrade.as_ref().unchecked_ref()));
    on_upgrade.forget();
}

fn transaction(
    database: &IdbDatabase,
    store: &str,
    mode: IdbTransactionMode,
) -> SyncResult<IdbTransaction> {
    database
        .transaction_with_str_and_mode(store, mode)
        .map_err(js_error)
}

async fn get_string(store: &IdbObjectStore, key: &str) -> SyncResult<Option<String>> {
    let request = store.get(&JsValue::from_str(key)).map_err(js_error)?;
    let value = request_value(request).await.map_err(js_error)?;
    if value.is_undefined() {
        return Ok(None);
    }
    value
        .as_string()
        .map(Some)
        .ok_or_else(|| SyncError::client("IndexedDB value was not a string"))
}

async fn put_string(store: &IdbObjectStore, key: &str, value: &str) -> SyncResult<()> {
    let request = store
        .put_with_key(&JsValue::from_str(value), &JsValue::from_str(key))
        .map_err(js_error)?;
    request_value(request).await.map_err(js_error)?;
    Ok(())
}

async fn load_stream_state(
    store: &IdbObjectStore,
    stream: SyncStreamName,
) -> SyncResult<LocalStreamSnapshot> {
    let value = get_string(store, stream.as_str()).await?;
    let Some(value) = value else {
        return Ok(LocalStreamSnapshot::empty(stream));
    };
    let snapshot: LocalStreamSnapshot = serde_json::from_str(&value)?;
    if snapshot.stream != stream {
        return Err(SyncError::client(format!(
            "IndexedDB sync state key {} contained stream {}",
            stream, snapshot.stream
        )));
    }
    Ok(snapshot)
}

async fn put_stream_state(store: &IdbObjectStore, state: &LocalStreamSnapshot) -> SyncResult<()> {
    put_string(store, state.stream.as_str(), &serde_json::to_string(state)?).await
}

fn request_value(request: IdbRequest) -> JsFuture {
    let promise = Promise::new(&mut |resolve: Function, reject: Function| {
        let success_request = request.clone();
        let reject_from_success = reject.clone();
        let on_success =
            Closure::<dyn FnMut(Event)>::new(move |_event| match success_request.result() {
                Ok(value) => {
                    let _ = resolve.call1(&JsValue::UNDEFINED, &value);
                }
                Err(err) => {
                    let _ = reject_from_success.call1(&JsValue::UNDEFINED, &err);
                }
            });
        request.set_onsuccess(Some(on_success.as_ref().unchecked_ref()));
        on_success.forget();

        let error_request = request.clone();
        let on_error = Closure::<dyn FnMut(Event)>::new(move |_event| {
            let _ = reject.call1(&JsValue::UNDEFINED, &request_error(&error_request));
        });
        request.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
    });
    JsFuture::from(promise)
}

fn transaction_done(transaction: &IdbTransaction) -> JsFuture {
    let transaction = transaction.clone();
    let promise = Promise::new(&mut |resolve: Function, reject: Function| {
        let on_complete = Closure::<dyn FnMut(Event)>::new(move |_event| {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        });
        transaction.set_oncomplete(Some(on_complete.as_ref().unchecked_ref()));
        on_complete.forget();

        let error_transaction = transaction.clone();
        let reject_from_error = reject.clone();
        let on_error = Closure::<dyn FnMut(Event)>::new(move |_event| {
            let _ = reject_from_error
                .call1(&JsValue::UNDEFINED, &transaction_error(&error_transaction));
        });
        transaction.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();

        let abort_transaction = transaction.clone();
        let on_abort = Closure::<dyn FnMut(Event)>::new(move |_event| {
            let _ = reject.call1(&JsValue::UNDEFINED, &transaction_error(&abort_transaction));
        });
        transaction.set_onabort(Some(on_abort.as_ref().unchecked_ref()));
        on_abort.forget();
    });
    JsFuture::from(promise)
}

async fn await_transaction(done: JsFuture) -> SyncResult<()> {
    done.await.map(drop).map_err(js_error)
}

fn request_error(request: &IdbRequest) -> JsValue {
    Reflect::get(request.as_ref(), &JsValue::from_str("error"))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .unwrap_or_else(|| JsValue::from_str("IndexedDB request failed"))
}

fn transaction_error(transaction: &IdbTransaction) -> JsValue {
    Reflect::get(transaction.as_ref(), &JsValue::from_str("error"))
        .ok()
        .filter(|value| !value.is_null() && !value.is_undefined())
        .unwrap_or_else(|| JsValue::from_str("IndexedDB transaction failed"))
}

fn rows_by_key(
    rows: Vec<SyncRow<serde_json::Value>>,
) -> BTreeMap<RowKey, SyncRow<serde_json::Value>> {
    rows.into_iter().map(|row| (row.key.clone(), row)).collect()
}

fn sorted_rows(rows: Vec<SyncRow<serde_json::Value>>) -> Vec<SyncRow<serde_json::Value>> {
    rows_by_key(rows).into_values().collect()
}

struct LocalChanges {
    had_reset: bool,
    items: Vec<pocopine_sync::SyncChange<serde_json::Value>>,
}

fn changes_after_last_reset(
    changes: Vec<pocopine_sync::SyncChange<serde_json::Value>>,
) -> LocalChanges {
    let Some(index) = changes
        .iter()
        .rposition(|change| change.op == SyncOp::Reset)
    else {
        return LocalChanges {
            had_reset: false,
            items: changes,
        };
    };

    LocalChanges {
        had_reset: true,
        items: changes.into_iter().skip(index).collect(),
    }
}

fn validate_database_name(database_name: String) -> SyncResult<String> {
    if database_name.trim().is_empty() || database_name.chars().any(char::is_control) {
        return Err(SyncError::client(format!(
            "invalid IndexedDB local-store database name: {database_name:?}"
        )));
    }
    Ok(database_name)
}

fn idb_gate() -> Rc<AsyncMutex<()>> {
    IDB_GATE.with(Clone::clone)
}

fn js_error(err: JsValue) -> SyncError {
    SyncError::client(js_value_to_string(&err))
}

fn js_value_to_string(value: &JsValue) -> String {
    if let Some(string) = value.as_string() {
        return string;
    }

    if let Ok(message) = Reflect::get(value, &JsValue::from_str("message")) {
        if let Some(message) = message.as_string() {
            return message;
        }
    }

    js_sys::JSON::stringify(value)
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_else(|| "unknown JavaScript error".to_string())
}
