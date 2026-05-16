use std::{cell::RefCell, collections::BTreeSet, fmt, rc::Rc};

use futures::lock::Mutex as AsyncMutex;
use js_sys::{Array, Reflect};
use pocopine_sync::{
    generate_sync_device_id, ClientMutation, LocalChangeBatch, LocalPushResult, LocalSnapshotBatch,
    LocalStreamSnapshot, MutationId, RowKey, RowVersion, SyncCollectionName, SyncCursor,
    SyncDeviceId, SyncError, SyncLocalFuture, SyncLocalIdentity, SyncLocalStore, SyncOp,
    SyncResult, SyncRow, SyncStreamName,
};
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};

use crate::schema::{
    BOOTSTRAP_SQL, DELETE_MUTATION_SQL, DELETE_ROW_SQL, DELETE_STREAM_ROWS_SQL, META_DEVICE_ID,
    META_NEXT_MUTATION_COUNTER, META_SCHEMA_VERSION, SCHEMA_VERSION, SELECT_PENDING_MUTATIONS_SQL,
    SELECT_ROWS_SQL, SELECT_STREAM_SQL, UPDATE_ROW_CONFLICT_SQL, UPSERT_MUTATION_SQL,
    UPSERT_ROW_SQL, UPSERT_STREAM_SQL,
};

const DEFAULT_DATABASE_NAME: &str = "pocopine_sync.sqlite3";

thread_local! {
    static SQLITE_GATE: Rc<AsyncMutex<()>> = Rc::new(AsyncMutex::new(()));
    static SQLITE_STARTED: RefCell<bool> = const { RefCell::new(false) };
    static SQLITE_CURRENT_DATABASE: RefCell<Option<String>> = const { RefCell::new(None) };
    static SQLITE_BOOTSTRAPPED: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

/// SQLite-backed [`SyncLocalStore`] for browser targets.
///
/// This adapter uses the official SQLite WASM worker through the `sqlite-wasm`
/// crate and opens a persistent OPFS database. SQLite WASM is a singleton in
/// the page process, so Pocopine serializes adapter operations through a global
/// browser-local gate before touching the worker.
#[derive(Clone)]
pub struct SqliteLocalStore {
    database_name: String,
}

impl SqliteLocalStore {
    /// Open the default browser OPFS database.
    pub fn new() -> Self {
        Self {
            database_name: DEFAULT_DATABASE_NAME.to_string(),
        }
    }

    /// Open a named browser OPFS database.
    ///
    /// The name is an OPFS filename, not a URL or path. Pocopine rejects empty,
    /// slash-containing, and control-character names so an app cannot
    /// accidentally select surprising storage.
    pub fn with_database_name(database_name: impl Into<String>) -> SyncResult<Self> {
        let database_name = validate_database_name(database_name.into())?;
        Ok(Self { database_name })
    }

    /// The OPFS database filename used by this store.
    pub fn database_name(&self) -> &str {
        &self.database_name
    }

    fn run<T: 'static>(
        &self,
        task: impl std::future::Future<Output = SyncResult<T>> + 'static,
    ) -> SyncLocalFuture<'static, T> {
        let database_name = self.database_name.clone();
        Box::pin(async move {
            let gate = sqlite_gate();
            let _guard = gate.lock().await;
            ensure_database(&database_name).await?;
            task.await
        })
    }
}

impl Default for SqliteLocalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SqliteLocalStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteLocalStore")
            .field("database_name", &self.database_name)
            .finish()
    }
}

impl SyncLocalStore for SqliteLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>> {
        self.run(load_identity())
    }

    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()> {
        self.run(save_identity(identity))
    }

    fn reserve_mutation_id(&self) -> SyncLocalFuture<'_, MutationId> {
        self.run(reserve_mutation_id())
    }

    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot> {
        self.run(hydrate_stream(stream.clone()))
    }

    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()> {
        self.run(save_snapshot(snapshot))
    }

    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()> {
        self.run(apply_changes(changes))
    }

    fn enqueue_mutation(
        &self,
        stream: &SyncStreamName,
        mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()> {
        self.run(enqueue_mutation(stream.clone(), mutation))
    }

    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        self.run(mark_push_result(result))
    }

    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        self.run(pending_mutations(stream.clone()))
    }
}

async fn ensure_database(database_name: &str) -> SyncResult<()> {
    let started = SQLITE_STARTED.with(|started| *started.borrow());
    if !started {
        sqlite_wasm::autostart_embedded().await.map_err(js_error)?;
        SQLITE_STARTED.with(|started| *started.borrow_mut() = true);
    }

    if sqlite_wasm::is_open() {
        let current = SQLITE_CURRENT_DATABASE.with(|current| current.borrow().clone());
        match current.as_deref() {
            Some(current) if current == database_name => {}
            Some(current) => {
                return Err(SyncError::client(format!(
                    "pocopine-sync-sqlite supports one open browser SQLite database per page; {current:?} is already open"
                )));
            }
            None => {
                return Err(SyncError::client(
                    "pocopine-sync-sqlite cannot attach because another SQLite WASM database is already open",
                ));
            }
        }
    } else {
        sqlite_wasm::open(database_name).await.map_err(js_error)?;
        SQLITE_CURRENT_DATABASE
            .with(|current| *current.borrow_mut() = Some(database_name.to_string()));
    }

    let bootstrapped = SQLITE_BOOTSTRAPPED.with(|names| names.borrow().contains(database_name));
    if !bootstrapped {
        bootstrap_schema().await?;
        SQLITE_BOOTSTRAPPED.with(|names| {
            names.borrow_mut().insert(database_name.to_string());
        });
    }

    Ok(())
}

async fn bootstrap_schema() -> SyncResult<()> {
    for sql in BOOTSTRAP_SQL {
        exec(sql, Vec::new()).await?;
    }
    validate_schema_version().await?;
    upsert_meta(META_SCHEMA_VERSION, &SCHEMA_VERSION.to_string()).await
}

async fn load_identity() -> SyncResult<Option<SyncLocalIdentity>> {
    let Some(device_id) = select_meta(META_DEVICE_ID).await? else {
        return Ok(None);
    };
    let next_counter = select_meta(META_NEXT_MUTATION_COUNTER)
        .await?
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                SyncError::backend(format!(
                    "invalid sync next mutation counter in sqlite store: {value}"
                ))
            })
        })
        .transpose()?
        .unwrap_or(1);

    SyncLocalIdentity::with_next_counter(SyncDeviceId::new(device_id)?, next_counter).map(Some)
}

async fn validate_schema_version() -> SyncResult<()> {
    let Some(existing) = select_meta(META_SCHEMA_VERSION).await? else {
        return Ok(());
    };
    let version = existing.parse::<u32>().map_err(|_| {
        SyncError::backend(format!(
            "invalid sync sqlite schema version in local store: {existing}"
        ))
    })?;
    if version != SCHEMA_VERSION {
        return Err(SyncError::backend(format!(
            "incompatible sync sqlite schema version: found {version}, expected {SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

async fn save_identity(identity: SyncLocalIdentity) -> SyncResult<()> {
    exec("BEGIN IMMEDIATE TRANSACTION", Vec::new()).await?;
    let result = async {
        upsert_meta(META_DEVICE_ID, identity.device_id.as_str()).await?;
        upsert_meta(
            META_NEXT_MUTATION_COUNTER,
            &identity.next_mutation_counter.to_string(),
        )
        .await
    }
    .await;
    finish_transaction(result).await
}

async fn reserve_mutation_id() -> SyncResult<MutationId> {
    exec("BEGIN IMMEDIATE TRANSACTION", Vec::new()).await?;
    let result = async {
        let identity = match load_identity().await? {
            Some(identity) => identity,
            None => SyncLocalIdentity::new(generate_sync_device_id()?),
        };
        let (id, advanced) = identity.reserve_mutation_id()?;
        upsert_meta(META_DEVICE_ID, advanced.device_id.as_str()).await?;
        upsert_meta(
            META_NEXT_MUTATION_COUNTER,
            &advanced.next_mutation_counter.to_string(),
        )
        .await?;
        Ok(id)
    }
    .await;
    finish_transaction_value(result).await
}

async fn hydrate_stream(stream: SyncStreamName) -> SyncResult<LocalStreamSnapshot> {
    let stream_rows =
        query_rows(SELECT_STREAM_SQL, vec![JsValue::from_str(stream.as_str())]).await?;
    let stream_meta = stream_rows.first();
    let rows = load_rows(&stream).await?;
    let pending_mutations = pending_mutations(stream.clone()).await?;

    let Some(meta) = stream_meta else {
        if rows.is_empty() && pending_mutations.is_empty() {
            return Ok(LocalStreamSnapshot::empty(stream));
        }

        return Ok(LocalStreamSnapshot {
            stream,
            collection: None,
            cursor: None,
            rows,
            pending_mutations,
        });
    };

    Ok(LocalStreamSnapshot {
        stream: stream.clone(),
        collection: Some(SyncCollectionName::new(required_string(
            meta,
            "collection",
        )?)?),
        cursor: optional_string(meta, "cursor")?
            .map(SyncCursor::new)
            .transpose()?,
        rows,
        pending_mutations,
    })
}

async fn save_snapshot(snapshot: LocalSnapshotBatch) -> SyncResult<()> {
    exec("BEGIN IMMEDIATE TRANSACTION", Vec::new()).await?;
    let result = async {
        let now = epoch_ms();
        upsert_stream(
            &snapshot.stream,
            &snapshot.collection,
            snapshot.cursor.as_ref(),
            now,
        )
        .await?;
        exec(
            DELETE_STREAM_ROWS_SQL,
            vec![JsValue::from_str(snapshot.stream.as_str())],
        )
        .await?;
        for row in snapshot.rows {
            upsert_row(&snapshot.stream, &row, now).await?;
        }
        Ok(())
    }
    .await;
    finish_transaction(result).await
}

async fn apply_changes(changes: LocalChangeBatch) -> SyncResult<()> {
    exec("BEGIN IMMEDIATE TRANSACTION", Vec::new()).await?;
    let result = async {
        let now = epoch_ms();
        let stream = changes.stream;
        upsert_stream(&stream, &changes.collection, changes.cursor.as_ref(), now).await?;
        let changes = changes_after_last_reset(changes.changes);
        if changes.had_reset {
            exec(
                DELETE_STREAM_ROWS_SQL,
                vec![JsValue::from_str(stream.as_str())],
            )
            .await?;
        }

        for change in changes.items {
            match change.op {
                SyncOp::Upsert => {
                    if let Some(row) = change.row {
                        upsert_row(&stream, &row, now).await?;
                    }
                }
                SyncOp::Delete => {
                    if let Some(key) = change.key {
                        exec(
                            DELETE_ROW_SQL,
                            vec![
                                JsValue::from_str(stream.as_str()),
                                JsValue::from_str(key.as_str()),
                            ],
                        )
                        .await?;
                    }
                }
                SyncOp::Reset => {
                    if let Some(row) = change.row {
                        upsert_row(&stream, &row, now).await?;
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    finish_transaction(result).await
}

async fn enqueue_mutation(
    stream: SyncStreamName,
    mutation: ClientMutation<serde_json::Value>,
) -> SyncResult<()> {
    exec(
        UPSERT_MUTATION_SQL,
        vec![
            JsValue::from_str(stream.as_str()),
            JsValue::from_str(mutation.id.as_str()),
            optional_param(mutation.key.as_ref().map(RowKey::as_str)),
            optional_param(mutation.base_version.as_ref().map(RowVersion::as_str)),
            JsValue::from_str(op_to_str(mutation.op)),
            JsValue::from_str(&serde_json::to_string(&mutation.payload)?),
            JsValue::from_f64(epoch_ms() as f64),
        ],
    )
    .await
}

async fn mark_push_result(result: LocalPushResult) -> SyncResult<()> {
    exec("BEGIN IMMEDIATE TRANSACTION", Vec::new()).await?;
    let tx_result = async {
        let now = epoch_ms();

        if let Some(collection) = result.collection.as_ref() {
            upsert_stream(&result.stream, collection, result.cursor.as_ref(), now).await?;
        } else if let Some(cursor) = result.cursor.as_ref() {
            exec(
                "update __pocopine_streams set cursor = ?2, updated_at_ms = ?3 where stream = ?1",
                vec![
                    JsValue::from_str(result.stream.as_str()),
                    JsValue::from_str(cursor.as_str()),
                    JsValue::from_f64(now as f64),
                ],
            )
            .await?;
            if stream_exists(&result.stream).await?.is_none() {
                return Err(SyncError::backend(format!(
                    "cannot persist sync push cursor for stream {} before stream metadata exists",
                    result.stream
                )));
            }
        }

        for id in result.accepted {
            delete_mutation(id.as_str()).await?;
        }

        for rejected in result.rejected {
            delete_mutation(rejected.mutation_id.as_str()).await?;
        }

        for conflict in result.conflicts {
            delete_mutation(conflict.mutation_id.as_str()).await?;
            if let Some(mut row) = conflict.server_row {
                row.pending = false;
                row.conflict = true;
                upsert_row(&result.stream, &row, now).await?;
            } else if let Some(key) = conflict.key {
                exec(
                    UPDATE_ROW_CONFLICT_SQL,
                    vec![
                        JsValue::from_str(result.stream.as_str()),
                        JsValue::from_str(key.as_str()),
                        JsValue::from_f64(now as f64),
                    ],
                )
                .await?;
            }
        }

        for mut row in result.rows {
            row.pending = false;
            row.conflict = false;
            upsert_row(&result.stream, &row, now).await?;
        }

        Ok(())
    }
    .await;
    finish_transaction(tx_result).await
}

async fn pending_mutations(
    stream: SyncStreamName,
) -> SyncResult<Vec<ClientMutation<serde_json::Value>>> {
    let rows = query_rows(
        SELECT_PENDING_MUTATIONS_SQL,
        vec![JsValue::from_str(stream.as_str())],
    )
    .await?;

    let mut mutations = Vec::new();
    for row in rows {
        let payload = optional_string(&row, "payload")?
            .map(|payload| serde_json::from_str(&payload))
            .transpose()?
            .unwrap_or(Value::Null);
        mutations.push(ClientMutation {
            id: pocopine_sync::MutationId::new(required_string(&row, "mutation_id")?)?,
            key: optional_string(&row, "row_key")?
                .map(RowKey::new)
                .transpose()?,
            base_version: optional_string(&row, "base_version")?
                .map(RowVersion::new)
                .transpose()?,
            op: op_from_str(&required_string(&row, "op")?)?,
            payload,
        });
    }
    Ok(mutations)
}

async fn load_rows(stream: &SyncStreamName) -> SyncResult<Vec<SyncRow<serde_json::Value>>> {
    let rows = query_rows(SELECT_ROWS_SQL, vec![JsValue::from_str(stream.as_str())]).await?;
    let mut out = Vec::new();
    for row in rows {
        out.push(SyncRow {
            key: RowKey::new(required_string(&row, "row_key")?)?,
            version: optional_string(&row, "version")?
                .map(RowVersion::new)
                .transpose()?,
            value: serde_json::from_str(&required_string(&row, "payload")?)?,
            pending: bool_from_int(&row, "pending")?,
            conflict: bool_from_int(&row, "conflict")?,
        });
    }
    Ok(out)
}

async fn upsert_stream(
    stream: &SyncStreamName,
    collection: &SyncCollectionName,
    cursor: Option<&SyncCursor>,
    updated_at_ms: i64,
) -> SyncResult<()> {
    exec(
        UPSERT_STREAM_SQL,
        vec![
            JsValue::from_str(stream.as_str()),
            JsValue::from_str(collection.as_str()),
            optional_param(cursor.map(SyncCursor::as_str)),
            JsValue::from_f64(SCHEMA_VERSION as f64),
            JsValue::from_f64(updated_at_ms as f64),
        ],
    )
    .await
}

async fn upsert_row(
    stream: &SyncStreamName,
    row: &SyncRow<serde_json::Value>,
    updated_at_ms: i64,
) -> SyncResult<()> {
    exec(
        UPSERT_ROW_SQL,
        vec![
            JsValue::from_str(stream.as_str()),
            JsValue::from_str(row.key.as_str()),
            optional_param(row.version.as_ref().map(RowVersion::as_str)),
            JsValue::from_str(&serde_json::to_string(&row.value)?),
            JsValue::from_f64(i32::from(row.pending) as f64),
            JsValue::from_f64(i32::from(row.conflict) as f64),
            JsValue::from_f64(updated_at_ms as f64),
        ],
    )
    .await
}

async fn upsert_meta(key: &str, value: &str) -> SyncResult<()> {
    exec(
        "insert into __pocopine_meta (key, value) values (?1, ?2)
         on conflict(key) do update set value = excluded.value",
        vec![JsValue::from_str(key), JsValue::from_str(value)],
    )
    .await
}

async fn select_meta(key: &str) -> SyncResult<Option<String>> {
    let rows = query_rows(
        "select value from __pocopine_meta where key = ?1",
        vec![JsValue::from_str(key)],
    )
    .await?;
    rows.first()
        .map(|row| required_string(row, "value"))
        .transpose()
}

async fn stream_exists(stream: &SyncStreamName) -> SyncResult<Option<()>> {
    let rows = query_rows(SELECT_STREAM_SQL, vec![JsValue::from_str(stream.as_str())]).await?;
    Ok(rows.first().map(|_| ()))
}

async fn delete_mutation(mutation_id: &str) -> SyncResult<()> {
    exec(DELETE_MUTATION_SQL, vec![JsValue::from_str(mutation_id)]).await
}

async fn finish_transaction(result: SyncResult<()>) -> SyncResult<()> {
    match result {
        Ok(()) => exec("COMMIT", Vec::new()).await,
        Err(err) => {
            let _ = exec("ROLLBACK", Vec::new()).await;
            Err(err)
        }
    }
}

async fn finish_transaction_value<T>(result: SyncResult<T>) -> SyncResult<T> {
    match result {
        Ok(value) => {
            exec("COMMIT", Vec::new()).await?;
            Ok(value)
        }
        Err(err) => {
            let _ = exec("ROLLBACK", Vec::new()).await;
            Err(err)
        }
    }
}

async fn exec(sql: &str, params: Vec<JsValue>) -> SyncResult<()> {
    sqlite_wasm::exec(sql, params).await.map_err(js_error)?;
    Ok(())
}

async fn query_rows(sql: &str, params: Vec<JsValue>) -> SyncResult<Vec<Value>> {
    let result = sqlite_wasm::query(sql, params).await.map_err(js_error)?;
    let result_obj = Reflect::get(&result, &JsValue::from_str("result")).map_err(js_error)?;
    if result_obj.is_undefined() || result_obj.is_null() {
        return Ok(Vec::new());
    }
    let rows = Reflect::get(&result_obj, &JsValue::from_str("resultRows")).map_err(js_error)?;
    if rows.is_undefined() || rows.is_null() {
        return Ok(Vec::new());
    }

    let rows = rows
        .dyn_into::<Array>()
        .map_err(|_| SyncError::backend("sqlite wasm query resultRows was not an array"))?;
    let mut out = Vec::with_capacity(rows.length() as usize);
    for index in 0..rows.length() {
        let row = rows.get(index);
        let value = serde_wasm_bindgen::from_value::<Value>(row)
            .map_err(|err| SyncError::backend(format!("invalid sqlite wasm row: {err}")))?;
        out.push(value);
    }
    Ok(out)
}

fn required_string(row: &Value, field: &'static str) -> SyncResult<String> {
    optional_string(row, field)?.ok_or_else(|| {
        SyncError::backend(format!("missing sqlite sync local-store field: {field}"))
    })
}

fn optional_string(row: &Value, field: &'static str) -> SyncResult<Option<String>> {
    match row.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(other) => Err(SyncError::backend(format!(
            "invalid sqlite sync local-store field {field}: expected string, got {other}"
        ))),
    }
}

fn bool_from_int(row: &Value, field: &'static str) -> SyncResult<bool> {
    match row.get(field) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(Value::Number(value)) => value
            .as_i64()
            .map(|value| value != 0)
            .ok_or_else(|| SyncError::backend(format!("invalid integer field: {field}"))),
        Some(other) => Err(SyncError::backend(format!(
            "invalid sqlite sync local-store field {field}: expected integer, got {other}"
        ))),
        None => Err(SyncError::backend(format!(
            "missing sqlite sync local-store field: {field}"
        ))),
    }
}

fn optional_param(value: Option<&str>) -> JsValue {
    value.map(JsValue::from_str).unwrap_or(JsValue::NULL)
}

fn op_to_str(op: SyncOp) -> &'static str {
    match op {
        SyncOp::Upsert => "upsert",
        SyncOp::Delete => "delete",
        SyncOp::Reset => "reset",
    }
}

fn op_from_str(value: &str) -> SyncResult<SyncOp> {
    match value {
        "upsert" => Ok(SyncOp::Upsert),
        "delete" => Ok(SyncOp::Delete),
        "reset" => Ok(SyncOp::Reset),
        other => Err(SyncError::backend(format!(
            "invalid sync mutation op in sqlite store: {other}"
        ))),
    }
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
    if database_name.trim() != database_name
        || database_name.is_empty()
        || database_name.len() > 255
        || database_name
            .chars()
            .any(|ch| ch.is_control() || ch == '/' || ch == '\\')
    {
        return Err(SyncError::backend(format!(
            "invalid sqlite local-store database name: {database_name:?}"
        )));
    }
    Ok(database_name)
}

fn sqlite_gate() -> Rc<AsyncMutex<()>> {
    SQLITE_GATE.with(Clone::clone)
}

fn epoch_ms() -> i64 {
    js_sys::Date::now().min(i64::MAX as f64) as i64
}

fn js_error(err: JsValue) -> SyncError {
    SyncError::client(js_value_to_string(err))
}

fn js_value_to_string(value: JsValue) -> String {
    value
        .as_string()
        .or_else(|| {
            js_sys::JSON::stringify(&value)
                .ok()
                .and_then(|text| text.as_string())
        })
        .unwrap_or_else(|| format!("{value:?}"))
}
