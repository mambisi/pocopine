use std::{
    future,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use pocopine_sync::{
    generate_sync_device_id, ClientMutation, LocalChangeBatch, LocalPendingMutation,
    LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot, MutationId, RowKey, RowVersion,
    SyncCollectionName, SyncCursor, SyncDeviceId, SyncError, SyncLocalFuture, SyncLocalIdentity,
    SyncLocalStore, SyncOp, SyncResult, SyncRow, SyncStreamName,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::schema::{
    BOOTSTRAP_SQL, CLEAR_ROW_CONFLICT_SQL, DELETE_MUTATION_SQL, DELETE_PENDING_FOR_ROW_SQL,
    DELETE_ROW_SQL, DELETE_STREAM_ROWS_SQL, META_DEVICE_ID, META_NEXT_MUTATION_COUNTER,
    META_SCHEMA_VERSION, SCHEMA_VERSION, SELECT_PENDING_MUTATIONS_SQL, SELECT_ROWS_SQL,
    SELECT_STREAM_SQL, UPDATE_ROW_CONFLICT_SQL, UPSERT_MUTATION_SQL, UPSERT_ROW_SQL,
    UPSERT_STREAM_SQL,
};

/// SQLite-backed [`SyncLocalStore`] for host/native targets.
#[derive(Clone)]
pub struct SqliteLocalStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteLocalStore {
    /// Open an in-memory SQLite local store.
    pub fn open_in_memory() -> SyncResult<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(sqlite_error)?)
    }

    /// Open or create a SQLite local store at `path`.
    pub fn open_path(path: impl AsRef<Path>) -> SyncResult<Self> {
        let conn = Connection::open(path).map_err(sqlite_error)?;
        configure_file_connection(&conn)?;
        Self::from_connection(conn)
    }

    /// Build a store from an existing SQLite connection and bootstrap schema.
    pub fn from_connection(mut conn: Connection) -> SyncResult<Self> {
        configure_connection(&conn)?;
        bootstrap_schema(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn with_conn<T>(&self, f: impl FnOnce(&mut Connection) -> SyncResult<T>) -> SyncResult<T> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| SyncError::backend("sqlite sync local store lock poisoned"))?;
        f(&mut conn)
    }

    fn ready<T: 'static>(result: SyncResult<T>) -> SyncLocalFuture<'static, T> {
        Box::pin(future::ready(result))
    }
}

impl SyncLocalStore for SqliteLocalStore {
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>> {
        Self::ready(self.with_conn(load_identity))
    }

    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_conn(|conn| save_identity(conn, identity)))
    }

    fn reserve_mutation_id(&self) -> SyncLocalFuture<'_, MutationId> {
        Self::ready(self.with_conn(reserve_mutation_id))
    }

    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot> {
        let stream = stream.clone();
        Self::ready(self.with_conn(|conn| hydrate_stream(conn, stream)))
    }

    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_conn(|conn| save_snapshot(conn, snapshot)))
    }

    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_conn(|conn| apply_changes(conn, changes)))
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
        let stream = stream.clone();
        Self::ready(self.with_conn(|conn| enqueue_mutation(conn, stream, pending)))
    }

    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_conn(|conn| mark_push_result(conn, result)))
    }

    fn clear_conflict(&self, stream: &SyncStreamName, key: &RowKey) -> SyncLocalFuture<'_, ()> {
        let stream = stream.clone();
        let key = key.clone();
        Self::ready(self.with_conn(|conn| clear_conflict(conn, &stream, &key)))
    }

    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        let stream = stream.clone();
        Self::ready(self.with_conn(|conn| pending_mutations(conn, &stream)))
    }

    fn purge_pending_for_row(
        &self,
        stream: &SyncStreamName,
        key: &RowKey,
    ) -> SyncLocalFuture<'_, usize> {
        let stream = stream.clone();
        let key = key.clone();
        Self::ready(self.with_conn(|conn| purge_pending_for_row(conn, &stream, &key)))
    }
}

fn bootstrap_schema(conn: &mut Connection) -> SyncResult<()> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    for sql in BOOTSTRAP_SQL {
        tx.execute_batch(sql).map_err(sqlite_error)?;
    }
    migrate_schema(&tx)?;
    validate_schema_version(&tx)?;
    upsert_meta(&tx, META_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    tx.commit().map_err(sqlite_error)
}

fn configure_connection(conn: &Connection) -> SyncResult<()> {
    conn.busy_timeout(Duration::from_millis(5_000))
        .map_err(sqlite_error)
}

fn configure_file_connection(conn: &Connection) -> SyncResult<()> {
    configure_connection(conn)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(sqlite_error)
}

fn validate_schema_version(tx: &Transaction<'_>) -> SyncResult<()> {
    let existing = tx
        .query_row(
            "select value from __pocopine_meta where key = ?1",
            params![META_SCHEMA_VERSION],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sqlite_error)?;

    if let Some(existing) = existing {
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
    }

    Ok(())
}

fn migrate_schema(tx: &Transaction<'_>) -> SyncResult<()> {
    let existing = select_meta_tx(tx, META_SCHEMA_VERSION)?;
    let Some(existing) = existing else {
        return Ok(());
    };
    let version = existing.parse::<u32>().map_err(|_| {
        SyncError::backend(format!(
            "invalid sync sqlite schema version in local store: {existing}"
        ))
    })?;
    if version == 2 && !column_exists(tx, "__pocopine_mutations", "optimistic_row")? {
        tx.execute(
            "alter table __pocopine_mutations add column optimistic_row text",
            [],
        )
        .map_err(sqlite_error)?;
    }
    if version == 2 {
        upsert_meta(tx, META_SCHEMA_VERSION, &SCHEMA_VERSION.to_string())?;
    }
    Ok(())
}

fn column_exists(tx: &Transaction<'_>, table: &str, column: &str) -> SyncResult<bool> {
    let mut stmt = tx
        .prepare(&format!("pragma table_info({table})"))
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error)?;
    for row in rows {
        if row.map_err(sqlite_error)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn load_identity(conn: &mut Connection) -> SyncResult<Option<SyncLocalIdentity>> {
    let Some(device_id) = select_meta(conn, META_DEVICE_ID)? else {
        return Ok(None);
    };
    let next_counter = select_meta(conn, META_NEXT_MUTATION_COUNTER)?
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

fn save_identity(conn: &mut Connection, identity: SyncLocalIdentity) -> SyncResult<()> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    upsert_meta(&tx, META_DEVICE_ID, identity.device_id.as_str())?;
    upsert_meta(
        &tx,
        META_NEXT_MUTATION_COUNTER,
        &identity.next_mutation_counter.to_string(),
    )?;
    tx.commit().map_err(sqlite_error)
}

fn reserve_mutation_id(conn: &mut Connection) -> SyncResult<MutationId> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    let identity = match load_identity_from_tx(&tx)? {
        Some(identity) => identity,
        None => SyncLocalIdentity::new(generate_sync_device_id()?),
    };
    let (id, advanced) = identity.reserve_mutation_id()?;
    upsert_meta(&tx, META_DEVICE_ID, advanced.device_id.as_str())?;
    upsert_meta(
        &tx,
        META_NEXT_MUTATION_COUNTER,
        &advanced.next_mutation_counter.to_string(),
    )?;
    tx.commit().map_err(sqlite_error)?;
    Ok(id)
}

fn load_identity_from_tx(tx: &Transaction<'_>) -> SyncResult<Option<SyncLocalIdentity>> {
    let Some(device_id) = select_meta_tx(tx, META_DEVICE_ID)? else {
        return Ok(None);
    };
    let next_counter = select_meta_tx(tx, META_NEXT_MUTATION_COUNTER)?
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

fn hydrate_stream(
    conn: &mut Connection,
    stream: SyncStreamName,
) -> SyncResult<LocalStreamSnapshot> {
    let stream_meta = conn
        .query_row(SELECT_STREAM_SQL, params![stream.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .optional()
        .map_err(sqlite_error)?;

    let rows = load_rows(conn, &stream)?;
    let pending_mutations = pending_mutation_records(conn, &stream)?;
    let Some((collection, cursor)) = stream_meta else {
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
        collection: Some(SyncCollectionName::new(collection)?),
        cursor: cursor.map(SyncCursor::new).transpose()?,
        rows,
        pending_mutations,
    })
}

fn save_snapshot(conn: &mut Connection, snapshot: LocalSnapshotBatch) -> SyncResult<()> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    let now = epoch_ms();
    upsert_stream(
        &tx,
        &snapshot.stream,
        &snapshot.collection,
        snapshot.cursor.as_ref(),
        now,
    )?;
    tx.execute(DELETE_STREAM_ROWS_SQL, params![snapshot.stream.as_str()])
        .map_err(sqlite_error)?;
    for row in snapshot.rows {
        upsert_row(&tx, &snapshot.stream, &row, now)?;
    }
    tx.commit().map_err(sqlite_error)
}

fn apply_changes(conn: &mut Connection, changes: LocalChangeBatch) -> SyncResult<()> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    let now = epoch_ms();
    let stream = changes.stream;
    upsert_stream(
        &tx,
        &stream,
        &changes.collection,
        changes.cursor.as_ref(),
        now,
    )?;
    let changes = changes_after_last_reset(changes.changes);
    if changes.had_reset {
        tx.execute(DELETE_STREAM_ROWS_SQL, params![stream.as_str()])
            .map_err(sqlite_error)?;
    }

    for change in changes.items {
        match change.op {
            SyncOp::Upsert => {
                if let Some(row) = change.row {
                    upsert_row(&tx, &stream, &row, now)?;
                }
            }
            SyncOp::Delete => {
                if let Some(key) = change.key {
                    tx.execute(DELETE_ROW_SQL, params![stream.as_str(), key.as_str()])
                        .map_err(sqlite_error)?;
                }
            }
            SyncOp::Reset => {
                if let Some(row) = change.row {
                    upsert_row(&tx, &stream, &row, now)?;
                }
            }
        }
    }
    tx.commit().map_err(sqlite_error)
}

fn enqueue_mutation(
    conn: &mut Connection,
    stream: SyncStreamName,
    pending: LocalPendingMutation,
) -> SyncResult<()> {
    let now = epoch_ms();
    let mutation = pending.mutation;
    let payload = serde_json::to_string(&mutation.payload)?;
    let optimistic_row = pending
        .optimistic_row
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    conn.execute(
        UPSERT_MUTATION_SQL,
        params![
            stream.as_str(),
            mutation.id.as_str(),
            mutation.key.as_ref().map(RowKey::as_str),
            mutation.base_version.as_ref().map(RowVersion::as_str),
            op_to_str(mutation.op),
            payload,
            optimistic_row,
            now,
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn mark_push_result(conn: &mut Connection, result: LocalPushResult) -> SyncResult<()> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    let now = epoch_ms();

    if let Some(collection) = result.collection.as_ref() {
        upsert_stream(&tx, &result.stream, collection, result.cursor.as_ref(), now)?;
    } else if let Some(cursor) = result.cursor.as_ref() {
        let updated = tx
            .execute(
                "update __pocopine_streams set cursor = ?2, updated_at_ms = ?3 where stream = ?1",
                params![result.stream.as_str(), cursor.as_str(), now],
            )
            .map_err(sqlite_error)?;
        if updated == 0 {
            return Err(SyncError::backend(format!(
                "cannot persist sync push cursor for stream {} before stream metadata exists",
                result.stream
            )));
        }
    }

    for id in result.accepted {
        delete_mutation(&tx, id.as_str())?;
    }

    for rejected in result.rejected {
        delete_mutation(&tx, rejected.mutation_id.as_str())?;
    }

    for conflict in result.conflicts {
        delete_mutation(&tx, conflict.mutation_id.as_str())?;
        if let Some(mut row) = conflict.server_row {
            row.pending = false;
            row.conflict = true;
            upsert_row(&tx, &result.stream, &row, now)?;
        } else if let Some(key) = conflict.key {
            tx.execute(
                UPDATE_ROW_CONFLICT_SQL,
                params![result.stream.as_str(), key.as_str(), now],
            )
            .map_err(sqlite_error)?;
        }
    }

    for mut row in result.rows {
        row.pending = false;
        row.conflict = false;
        upsert_row(&tx, &result.stream, &row, now)?;
    }

    tx.commit().map_err(sqlite_error)
}

fn clear_conflict(conn: &mut Connection, stream: &SyncStreamName, key: &RowKey) -> SyncResult<()> {
    conn.execute(
        CLEAR_ROW_CONFLICT_SQL,
        params![stream.as_str(), key.as_str(), epoch_ms()],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn purge_pending_for_row(
    conn: &mut Connection,
    stream: &SyncStreamName,
    key: &RowKey,
) -> SyncResult<usize> {
    let affected = conn
        .execute(
            DELETE_PENDING_FOR_ROW_SQL,
            params![stream.as_str(), key.as_str()],
        )
        .map_err(sqlite_error)?;
    Ok(affected)
}

fn pending_mutations(
    conn: &mut Connection,
    stream: &SyncStreamName,
) -> SyncResult<Vec<ClientMutation<serde_json::Value>>> {
    Ok(pending_mutation_records(conn, stream)?
        .into_iter()
        .map(|pending| pending.mutation)
        .collect())
}

fn pending_mutation_records(
    conn: &mut Connection,
    stream: &SyncStreamName,
) -> SyncResult<Vec<LocalPendingMutation>> {
    let mut stmt = conn
        .prepare(SELECT_PENDING_MUTATIONS_SQL)
        .map_err(sqlite_error)?;
    let rows = stmt
        .query_map(params![stream.as_str()], |row| {
            let mutation_id: String = row.get(0)?;
            let row_key: Option<String> = row.get(1)?;
            let base_version: Option<String> = row.get(2)?;
            let op: String = row.get(3)?;
            let payload: Option<String> = row.get(4)?;
            let optimistic_row: Option<String> = row.get(5)?;
            Ok((
                mutation_id,
                row_key,
                base_version,
                op,
                payload,
                optimistic_row,
            ))
        })
        .map_err(sqlite_error)?;

    let mut mutations = Vec::new();
    for row in rows {
        let (mutation_id, row_key, base_version, op, payload, optimistic_row) =
            row.map_err(sqlite_error)?;
        mutations.push(LocalPendingMutation {
            mutation: ClientMutation {
                id: pocopine_sync::MutationId::new(mutation_id)?,
                key: row_key.map(RowKey::new).transpose()?,
                base_version: base_version.map(RowVersion::new).transpose()?,
                op: op_from_str(&op)?,
                payload: payload
                    .map(|payload| serde_json::from_str(&payload))
                    .transpose()?
                    .unwrap_or(serde_json::Value::Null),
            },
            optimistic_row: optimistic_row
                .map(|row| serde_json::from_str(&row))
                .transpose()?,
        });
    }
    Ok(mutations)
}

fn load_rows(
    conn: &mut Connection,
    stream: &SyncStreamName,
) -> SyncResult<Vec<SyncRow<serde_json::Value>>> {
    let mut stmt = conn.prepare(SELECT_ROWS_SQL).map_err(sqlite_error)?;
    let rows = stmt
        .query_map(params![stream.as_str()], |row| {
            let row_key: String = row.get(0)?;
            let version: Option<String> = row.get(1)?;
            let payload: String = row.get(2)?;
            let pending: i64 = row.get(3)?;
            let conflict: i64 = row.get(4)?;
            Ok((row_key, version, payload, pending, conflict))
        })
        .map_err(sqlite_error)?;

    let mut out = Vec::new();
    for row in rows {
        let (row_key, version, payload, pending, conflict) = row.map_err(sqlite_error)?;
        out.push(SyncRow {
            key: RowKey::new(row_key)?,
            version: version.map(RowVersion::new).transpose()?,
            value: serde_json::from_str(&payload)?,
            pending: pending != 0,
            conflict: conflict != 0,
        });
    }
    Ok(out)
}

fn upsert_stream(
    tx: &Transaction<'_>,
    stream: &SyncStreamName,
    collection: &SyncCollectionName,
    cursor: Option<&SyncCursor>,
    updated_at_ms: i64,
) -> SyncResult<()> {
    tx.execute(
        UPSERT_STREAM_SQL,
        params![
            stream.as_str(),
            collection.as_str(),
            cursor.map(SyncCursor::as_str),
            SCHEMA_VERSION,
            updated_at_ms,
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn upsert_row(
    tx: &Transaction<'_>,
    stream: &SyncStreamName,
    row: &SyncRow<serde_json::Value>,
    updated_at_ms: i64,
) -> SyncResult<()> {
    tx.execute(
        UPSERT_ROW_SQL,
        params![
            stream.as_str(),
            row.key.as_str(),
            row.version.as_ref().map(RowVersion::as_str),
            serde_json::to_string(&row.value)?,
            i64::from(row.pending),
            i64::from(row.conflict),
            updated_at_ms,
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn upsert_meta(tx: &Transaction<'_>, key: &str, value: &str) -> SyncResult<()> {
    tx.execute(
        "insert into __pocopine_meta (key, value) values (?1, ?2)
         on conflict(key) do update set value = excluded.value",
        params![key, value],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn select_meta(conn: &Connection, key: &str) -> SyncResult<Option<String>> {
    conn.query_row(
        "select value from __pocopine_meta where key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(sqlite_error)
}

fn select_meta_tx(tx: &Transaction<'_>, key: &str) -> SyncResult<Option<String>> {
    tx.query_row(
        "select value from __pocopine_meta where key = ?1",
        params![key],
        |row| row.get(0),
    )
    .optional()
    .map_err(sqlite_error)
}

fn delete_mutation(tx: &Transaction<'_>, mutation_id: &str) -> SyncResult<()> {
    tx.execute(DELETE_MUTATION_SQL, params![mutation_id])
        .map_err(sqlite_error)?;
    Ok(())
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

fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn sqlite_error(err: rusqlite::Error) -> SyncError {
    SyncError::backend(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use pocopine_sync::{
        LocalSnapshotBatch, MutationId, SyncChange, SyncCollectionName, SyncConflict, SyncCursor,
        SyncRejectedMutation,
    };

    #[test]
    fn sqlite_store_persists_identity() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let identity =
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 9)
                .unwrap();

        assert!(block(store.load_identity()).unwrap().is_none());
        block(store.save_identity(identity.clone())).unwrap();

        assert_eq!(block(store.load_identity()).unwrap(), Some(identity));
    }

    #[test]
    fn sqlite_store_reserves_mutation_ids_and_persists_counter() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let identity =
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_abc").unwrap(), 9)
                .unwrap();
        block(store.save_identity(identity)).unwrap();

        let first = block(store.reserve_mutation_id()).unwrap();
        let second = block(store.reserve_mutation_id()).unwrap();

        assert_eq!(first.as_str(), "device_abc:9");
        assert_eq!(second.as_str(), "device_abc:10");
        assert_eq!(
            block(store.load_identity())
                .unwrap()
                .unwrap()
                .next_mutation_counter,
            11
        );
    }

    #[test]
    fn sqlite_store_reserve_creates_identity_when_missing() {
        let store = SqliteLocalStore::open_in_memory().unwrap();

        let id = block(store.reserve_mutation_id()).unwrap();
        let identity = block(store.load_identity()).unwrap().unwrap();

        assert!(id.as_str().starts_with(identity.device_id.as_str()));
        assert!(id.as_str().ends_with(":1"));
        assert_eq!(identity.next_mutation_counter, 2);
    }

    #[test]
    fn sqlite_store_saves_snapshot_and_hydrates_rows() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        let row = SyncRow::new("post_1", serde_json::json!({"title": "Cached"}))
            .unwrap()
            .version("row_1")
            .unwrap();

        block(store.save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![row.clone()],
            Some(SyncCursor::new("cursor_1").unwrap()),
        )))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();

        assert_eq!(snapshot.collection, Some(collection));
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_1");
        assert_eq!(snapshot.rows, vec![row]);
    }

    #[test]
    fn sqlite_store_applies_changes_transactionally() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        let row = SyncRow::new("post_1", serde_json::json!({"title": "Cached"})).unwrap();
        block(store.save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![row],
            Some(SyncCursor::new("cursor_1").unwrap()),
        )))
        .unwrap();

        let updated = SyncRow::new("post_1", serde_json::json!({"title": "Updated"}))
            .unwrap()
            .version("row_2")
            .unwrap();
        let new_row = SyncRow::new("post_2", serde_json::json!({"title": "New"})).unwrap();
        block(store.apply_changes(LocalChangeBatch::new(
            stream.clone(),
            collection,
            vec![
                SyncChange {
                    stream: stream.clone(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: Some(RowKey::new("post_1").unwrap()),
                    op: SyncOp::Upsert,
                    row: Some(updated.clone()),
                    cursor: SyncCursor::new("cursor_2").unwrap(),
                },
                SyncChange {
                    stream: stream.clone(),
                    collection: SyncCollectionName::new("posts").unwrap(),
                    key: Some(RowKey::new("post_2").unwrap()),
                    op: SyncOp::Upsert,
                    row: Some(new_row.clone()),
                    cursor: SyncCursor::new("cursor_2").unwrap(),
                },
            ],
            Some(SyncCursor::new("cursor_2").unwrap()),
        )))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();

        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_2");
        assert_eq!(snapshot.rows, vec![updated, new_row]);
    }

    #[test]
    fn sqlite_store_replays_pending_mutations_and_clears_acceptance() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let mutation = ClientMutation {
            id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: Some(RowVersion::new("row_1").unwrap()),
            payload: serde_json::json!({"title": "Saved"}),
        };

        block(store.enqueue_mutation(&stream, mutation.clone())).unwrap();
        assert_eq!(
            block(store.pending_mutations(&stream)).unwrap(),
            vec![mutation]
        );

        block(store.mark_push_result(LocalPushResult {
            stream: stream.clone(),
            collection: Some(SyncCollectionName::new("posts").unwrap()),
            accepted: vec![MutationId::new("device_abc:1").unwrap()],
            rejected: Vec::new(),
            rows: vec![SyncRow::new("post_1", serde_json::json!({"title": "Saved"})).unwrap()],
            conflicts: Vec::new(),
            cursor: Some(SyncCursor::new("cursor_2").unwrap()),
        }))
        .unwrap();

        assert!(block(store.pending_mutations(&stream)).unwrap().is_empty());
        assert_eq!(block(store.hydrate_stream(&stream)).unwrap().rows.len(), 1);
    }

    #[test]
    fn sqlite_store_round_trips_pending_optimistic_rows() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let mutation = ClientMutation {
            id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({
                "op": "create",
                "payload": {"id": "post_1", "draft": {"title": "Envelope only"}}
            }),
        };
        let optimistic = SyncRow::new(
            "post_1",
            serde_json::json!({"id": "post_1", "title": "Visible"}),
        )
        .unwrap();

        block(
            store.enqueue_pending_mutation(
                &stream,
                LocalPendingMutation::new(mutation.clone())
                    .with_optimistic_row(Some(optimistic.clone())),
            ),
        )
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(snapshot.pending_mutations[0].mutation, mutation);
        assert_eq!(
            snapshot.pending_mutations[0].optimistic_row.as_ref(),
            Some(&optimistic)
        );
        assert_eq!(
            block(store.pending_mutations(&stream)).unwrap(),
            vec![snapshot.pending_mutations[0].mutation.clone()]
        );
    }

    #[test]
    fn sqlite_store_persists_push_cursor_before_snapshot_when_collection_is_present() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();

        block(store.mark_push_result(LocalPushResult {
            stream: stream.clone(),
            collection: Some(SyncCollectionName::new("posts").unwrap()),
            accepted: vec![MutationId::new("device_abc:1").unwrap()],
            rejected: Vec::new(),
            rows: vec![SyncRow::new("post_1", serde_json::json!({"title": "Saved"}))
                .unwrap()
                .version("row_1")
                .unwrap()],
            conflicts: Vec::new(),
            cursor: Some(SyncCursor::new("cursor_1").unwrap()),
        }))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(snapshot.collection.unwrap().as_str(), "posts");
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_1");
        assert_eq!(snapshot.rows.len(), 1);
    }

    #[test]
    fn sqlite_store_persists_rejections_and_conflicts_as_terminal_outcomes() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        block(store.enqueue_mutation(
            &stream,
            ClientMutation {
                id: MutationId::new("device_abc:1").unwrap(),
                key: Some(RowKey::new("post_1").unwrap()),
                op: SyncOp::Upsert,
                base_version: None,
                payload: serde_json::json!({"title": "Draft"}),
            },
        ))
        .unwrap();
        block(store.enqueue_mutation(
            &stream,
            ClientMutation {
                id: MutationId::new("device_abc:2").unwrap(),
                key: Some(RowKey::new("post_2").unwrap()),
                op: SyncOp::Upsert,
                base_version: None,
                payload: serde_json::json!({"title": "Draft"}),
            },
        ))
        .unwrap();

        block(store.mark_push_result(LocalPushResult {
            stream: stream.clone(),
            collection: Some(SyncCollectionName::new("posts").unwrap()),
            accepted: Vec::new(),
            rejected: vec![SyncRejectedMutation {
                mutation_id: MutationId::new("device_abc:1").unwrap(),
                key: Some(RowKey::new("post_1").unwrap()),
                reason: "invalid".to_string(),
            }],
            rows: Vec::new(),
            conflicts: vec![SyncConflict {
                mutation_id: MutationId::new("device_abc:2").unwrap(),
                key: Some(RowKey::new("post_2").unwrap()),
                server_row: Some(
                    SyncRow::new("post_2", serde_json::json!({"title": "Server"})).unwrap(),
                ),
                reason: "stale".to_string(),
            }],
            cursor: None,
        }))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert!(block(store.pending_mutations(&stream)).unwrap().is_empty());
        assert_eq!(snapshot.rows.len(), 1);
        assert!(snapshot.rows[0].conflict);
    }

    #[test]
    fn sqlite_store_clears_conflict_rows() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();

        block(store.mark_push_result(LocalPushResult {
            stream: stream.clone(),
            collection: Some(SyncCollectionName::new("posts").unwrap()),
            accepted: Vec::new(),
            rejected: Vec::new(),
            rows: Vec::new(),
            conflicts: vec![SyncConflict {
                mutation_id: MutationId::new("device_abc:1").unwrap(),
                key: Some(RowKey::new("post_1").unwrap()),
                server_row: Some(
                    SyncRow::new("post_1", serde_json::json!({"title": "Server"})).unwrap(),
                ),
                reason: "stale".to_string(),
            }],
            cursor: None,
        }))
        .unwrap();

        block(store.clear_conflict(&stream, &RowKey::new("post_1").unwrap())).unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(snapshot.rows.len(), 1);
        assert!(!snapshot.rows[0].conflict);
    }

    #[test]
    fn sqlite_store_rejects_incompatible_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table __pocopine_meta (
                key text primary key,
                value text not null
            );
            insert into __pocopine_meta (key, value) values ('schema_version', '999');",
        )
        .unwrap();

        let err = match SqliteLocalStore::from_connection(conn) {
            Ok(_) => panic!("schema version mismatch should fail"),
            Err(err) => err,
        };

        assert!(err
            .to_string()
            .contains("incompatible sync sqlite schema version"));
    }

    #[test]
    fn sqlite_store_migrates_v2_pending_mutations_to_optimistic_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table __pocopine_meta (
                key text primary key,
                value text not null
            );
            create table __pocopine_streams (
                stream text primary key,
                collection text not null,
                cursor text,
                schema_version integer not null,
                updated_at_ms integer not null
            );
            create table __pocopine_rows (
                stream text not null,
                row_key text not null,
                version text,
                payload text not null,
                pending integer not null default 0,
                conflict integer not null default 0,
                updated_at_ms integer not null,
                primary key (stream, row_key)
            );
            create table __pocopine_mutations (
                enqueue_seq integer primary key autoincrement,
                stream text not null,
                mutation_id text not null unique,
                row_key text,
                base_version text,
                op text not null,
                payload text,
                status text not null,
                error text,
                created_at_ms integer not null,
                updated_at_ms integer not null
            );
            insert into __pocopine_meta (key, value) values ('schema_version', '2');",
        )
        .unwrap();

        let store = SqliteLocalStore::from_connection(conn).unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let mutation = ClientMutation {
            id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"op": "create"}),
        };
        let optimistic = SyncRow::new("post_1", serde_json::json!({"title": "Visible"})).unwrap();

        block(store.enqueue_pending_mutation(
            &stream,
            LocalPendingMutation::new(mutation).with_optimistic_row(Some(optimistic.clone())),
        ))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(
            snapshot.pending_mutations[0].optimistic_row.as_ref(),
            Some(&optimistic)
        );
        store
            .with_conn(|conn| {
                assert_eq!(
                    select_meta(conn, META_SCHEMA_VERSION)?,
                    Some(SCHEMA_VERSION.to_string())
                );
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn sqlite_store_save_snapshot_replaces_previous_rows() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();

        block(store.save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![
                SyncRow::new("post_1", serde_json::json!({"title": "Old1"})).unwrap(),
                SyncRow::new("post_2", serde_json::json!({"title": "Old2"})).unwrap(),
            ],
            Some(SyncCursor::new("cursor_1").unwrap()),
        )))
        .unwrap();

        let replacement =
            SyncRow::new("post_3", serde_json::json!({"title": "Only survivor"})).unwrap();
        block(store.save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection,
            vec![replacement.clone()],
            Some(SyncCursor::new("cursor_2").unwrap()),
        )))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(snapshot.rows, vec![replacement]);
        assert_eq!(snapshot.cursor.unwrap().as_str(), "cursor_2");
    }

    #[test]
    fn sqlite_store_apply_changes_reset_then_upsert_keeps_only_post_reset_rows() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        block(store.save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![
                SyncRow::new("old_1", serde_json::json!({"title": "Old1"})).unwrap(),
                SyncRow::new("old_2", serde_json::json!({"title": "Old2"})).unwrap(),
            ],
            Some(SyncCursor::new("cursor_1").unwrap()),
        )))
        .unwrap();

        let cursor = SyncCursor::new("cursor_2").unwrap();
        let after_reset =
            SyncRow::new("post_after", serde_json::json!({"title": "After"})).unwrap();
        block(store.apply_changes(LocalChangeBatch::new(
            stream.clone(),
            collection.clone(),
            vec![
                SyncChange {
                    stream: stream.clone(),
                    collection: collection.clone(),
                    key: Some(RowKey::new("ignored").unwrap()),
                    op: SyncOp::Upsert,
                    row: Some(
                        SyncRow::new("ignored", serde_json::json!({"title": "Pre-reset"})).unwrap(),
                    ),
                    cursor: cursor.clone(),
                },
                SyncChange {
                    stream: stream.clone(),
                    collection: collection.clone(),
                    key: None,
                    op: SyncOp::Reset,
                    row: None,
                    cursor: cursor.clone(),
                },
                SyncChange {
                    stream: stream.clone(),
                    collection,
                    key: Some(after_reset.key.clone()),
                    op: SyncOp::Upsert,
                    row: Some(after_reset.clone()),
                    cursor: cursor.clone(),
                },
            ],
            Some(cursor),
        )))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(snapshot.rows, vec![after_reset]);
    }

    #[test]
    fn sqlite_store_mark_push_cursor_only_without_prior_meta_errors() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();

        let err = block(store.mark_push_result(LocalPushResult {
            stream: stream.clone(),
            collection: None,
            accepted: Vec::new(),
            rejected: Vec::new(),
            rows: Vec::new(),
            conflicts: Vec::new(),
            cursor: Some(SyncCursor::new("orphan_cursor").unwrap()),
        }))
        .unwrap_err();

        assert!(
            err.to_string().contains("cannot persist sync push cursor"),
            "expected error about missing stream metadata, got: {err}"
        );
    }

    #[test]
    fn sqlite_store_pending_mutations_preserve_enqueue_order() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        for id in ["device_abc:10", "device_abc:2", "device_abc:1"] {
            block(store.enqueue_mutation(
                &stream,
                ClientMutation {
                    id: MutationId::new(id).unwrap(),
                    key: Some(RowKey::new("post_1").unwrap()),
                    op: SyncOp::Upsert,
                    base_version: None,
                    payload: serde_json::json!({"id": id}),
                },
            ))
            .unwrap();
        }
        store
            .with_conn(|conn| {
                conn.execute("update __pocopine_mutations set created_at_ms = 42", [])
                    .map_err(sqlite_error)?;
                Ok(())
            })
            .unwrap();

        let pending = block(store.pending_mutations(&stream)).unwrap();
        let ids: Vec<_> = pending.iter().map(|m| m.id.as_str().to_string()).collect();
        assert_eq!(ids, vec!["device_abc:10", "device_abc:2", "device_abc:1"]);
    }

    #[test]
    fn sqlite_store_apply_changes_delete_missing_key_is_noop() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let collection = SyncCollectionName::new("posts").unwrap();
        block(store.save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![SyncRow::new("post_1", serde_json::json!({"title": "Kept"})).unwrap()],
            None,
        )))
        .unwrap();

        let cursor = SyncCursor::new("cursor_2").unwrap();
        block(store.apply_changes(LocalChangeBatch::new(
            stream.clone(),
            collection.clone(),
            vec![SyncChange {
                stream: stream.clone(),
                collection,
                key: Some(RowKey::new("never_existed").unwrap()),
                op: SyncOp::Delete,
                row: None,
                cursor: cursor.clone(),
            }],
            Some(cursor),
        )))
        .unwrap();

        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].key.as_str(), "post_1");
    }

    #[test]
    fn sqlite_store_hydrate_returns_empty_for_unknown_stream() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("never_seen").unwrap();
        let snapshot = block(store.hydrate_stream(&stream)).unwrap();
        assert_eq!(snapshot.stream, stream);
        assert!(snapshot.collection.is_none());
        assert!(snapshot.cursor.is_none());
        assert!(snapshot.rows.is_empty());
        assert!(snapshot.pending_mutations.is_empty());
    }

    #[test]
    fn sqlite_purge_pending_for_row_drops_only_target_row_pendings() {
        // Cover the durability + scope contract: purge a row's pending
        // mutations and confirm only that row's queue is affected,
        // and that hydration (via a fresh connection) reflects the
        // purge. This is the test that distinguishes a real durable
        // purge from a cosmetic in-memory clear.
        let path = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        let store = SqliteLocalStore::open_path(&path).unwrap();
        let stream = SyncStreamName::new("posts").unwrap();

        let post_a_first = ClientMutation {
            id: MutationId::new("device_abc:1").unwrap(),
            key: Some(RowKey::new("post_a").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"title": "A1"}),
        };
        let post_a_second = ClientMutation {
            id: MutationId::new("device_abc:2").unwrap(),
            key: Some(RowKey::new("post_a").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"title": "A2"}),
        };
        let post_b = ClientMutation {
            id: MutationId::new("device_abc:3").unwrap(),
            key: Some(RowKey::new("post_b").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::json!({"title": "B"}),
        };
        for m in [&post_a_first, &post_a_second, &post_b] {
            block(store.enqueue_mutation(&stream, m.clone())).unwrap();
        }

        let purged =
            block(store.purge_pending_for_row(&stream, &RowKey::new("post_a").unwrap())).unwrap();
        assert_eq!(purged, 2);

        // Drop the live store and reopen against the same SQLite file.
        // If the durable layer leaked anything we'll see it now.
        drop(store);
        let reopened = SqliteLocalStore::open_path(&path).unwrap();
        let snapshot = block(reopened.hydrate_stream(&stream)).unwrap();
        let surviving_ids: Vec<_> = snapshot
            .pending_mutations
            .iter()
            .map(|p| p.mutation.id.as_str().to_owned())
            .collect();
        assert_eq!(surviving_ids, vec!["device_abc:3"]);
    }

    #[test]
    fn sqlite_purge_pending_for_row_idempotent_returns_zero_for_unknown_row() {
        let store = SqliteLocalStore::open_in_memory().unwrap();
        let stream = SyncStreamName::new("posts").unwrap();
        let purged =
            block(store.purge_pending_for_row(&stream, &RowKey::new("never").unwrap())).unwrap();
        assert_eq!(purged, 0);
    }

    fn block<T>(future: SyncLocalFuture<'_, T>) -> SyncResult<T> {
        let waker = std::task::Waker::noop();
        let mut cx = std::task::Context::from_waker(waker);
        let mut future = future;
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => {
                panic!("sqlite local store futures must be immediately ready on host")
            }
        }
    }
}
