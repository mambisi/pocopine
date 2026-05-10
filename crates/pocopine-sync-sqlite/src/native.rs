use std::{
    future,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use pocopine_sync::{
    ClientMutation, LocalChangeBatch, LocalPushResult, LocalSnapshotBatch, LocalStreamSnapshot,
    RowKey, RowVersion, SyncCollectionName, SyncCursor, SyncDeviceId, SyncError, SyncLocalFuture,
    SyncLocalIdentity, SyncLocalStore, SyncOp, SyncResult, SyncRow, SyncStreamName,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::schema::{
    BOOTSTRAP_SQL, DELETE_MUTATION_SQL, DELETE_ROW_SQL, DELETE_STREAM_ROWS_SQL, META_DEVICE_ID,
    META_NEXT_MUTATION_COUNTER, META_SCHEMA_VERSION, SCHEMA_VERSION, SELECT_PENDING_MUTATIONS_SQL,
    SELECT_ROWS_SQL, SELECT_STREAM_SQL, UPDATE_ROW_CONFLICT_SQL, UPSERT_MUTATION_SQL,
    UPSERT_ROW_SQL, UPSERT_STREAM_SQL,
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
        let stream = stream.clone();
        Self::ready(self.with_conn(|conn| enqueue_mutation(conn, stream, mutation)))
    }

    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()> {
        Self::ready(self.with_conn(|conn| mark_push_result(conn, result)))
    }

    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>> {
        let stream = stream.clone();
        Self::ready(self.with_conn(|conn| pending_mutations(conn, &stream)))
    }
}

fn bootstrap_schema(conn: &mut Connection) -> SyncResult<()> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    for sql in BOOTSTRAP_SQL {
        tx.execute_batch(sql).map_err(sqlite_error)?;
    }
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
    let pending_mutations = pending_mutations(conn, &stream)?;
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
    mutation: ClientMutation<serde_json::Value>,
) -> SyncResult<()> {
    let now = epoch_ms();
    let payload = serde_json::to_string(&mutation.payload)?;
    conn.execute(
        UPSERT_MUTATION_SQL,
        params![
            stream.as_str(),
            mutation.id.as_str(),
            mutation.key.as_ref().map(RowKey::as_str),
            mutation.base_version.as_ref().map(RowVersion::as_str),
            op_to_str(mutation.op),
            payload,
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

fn pending_mutations(
    conn: &mut Connection,
    stream: &SyncStreamName,
) -> SyncResult<Vec<ClientMutation<serde_json::Value>>> {
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
            Ok((mutation_id, row_key, base_version, op, payload))
        })
        .map_err(sqlite_error)?;

    let mut mutations = Vec::new();
    for row in rows {
        let (mutation_id, row_key, base_version, op, payload) = row.map_err(sqlite_error)?;
        mutations.push(ClientMutation {
            id: pocopine_sync::MutationId::new(mutation_id)?,
            key: row_key.map(RowKey::new).transpose()?,
            base_version: base_version.map(RowVersion::new).transpose()?,
            op: op_from_str(&op)?,
            payload: payload
                .map(|payload| serde_json::from_str(&payload))
                .transpose()?
                .unwrap_or(serde_json::Value::Null),
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
    fn sqlite_store_rejects_incompatible_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table __pocopine_meta (
                key text primary key,
                value text not null
            );
            insert into __pocopine_meta (key, value) values ('schema_version', '2');",
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
