//! SQLite-backed sync streams for the Keep example server.
//!
//! This is intentionally small, but it is a real SQLite sync source:
//! rows, row versions, incremental changes, and accepted mutation ids are
//! all persisted in rusqlite. The browser still exercises the
//! `pocopine-sync-sqlite` local cache; this module is the server-side
//! authority for the example app.

use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pocopine_server::auth::RequestContext;
use pocopine_sync::{
    ClientMutation, RowKey, RowVersion, SyncBoxFuture, SyncChange, SyncCollectionName,
    SyncConflict, SyncCursor, SyncError, SyncOp, SyncPullRequest, SyncPullResponse,
    SyncPushRequest, SyncPushResponse, SyncRejectedMutation, SyncResult, SyncRow, SyncStreamName,
    SyncStreamSource,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

use crate::{KeepNote, KeepTag};

const SCHEMA_VERSION: &str = "2";
const LEGACY_SCHEMA_VERSION: &str = "1";
const META_SCHEMA_VERSION: &str = "schema_version";
const META_CURSOR_VERSION: &str = "cursor_version";

/// Where the keep example writes notes. Set `POCOPINE_KEEP_NOTES_DB_PATH` to
/// isolate test/dev runs; otherwise the database sits under the system temp
/// dir so it does not pollute the source tree but survives `cargo run` cycles.
pub fn default_keep_notes_path() -> PathBuf {
    default_keep_sqlite_path("POCOPINE_KEEP_NOTES_DB_PATH", "pocopine_keep_notes.sqlite3")
}

/// Where the keep example writes tags. Set `POCOPINE_KEEP_TAGS_DB_PATH` to
/// isolate test/dev runs.
pub fn default_keep_tags_path() -> PathBuf {
    default_keep_sqlite_path("POCOPINE_KEEP_TAGS_DB_PATH", "pocopine_keep_tags.sqlite3")
}

fn default_keep_sqlite_path(env_key: &str, file_name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(env_key)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
    {
        return path;
    }
    std::env::temp_dir().join(file_name)
}

#[derive(Clone)]
pub struct SqliteSyncStream<T> {
    stream: SyncStreamName,
    collection: SyncCollectionName,
    path: PathBuf,
    lock: Arc<Mutex<()>>,
    _marker: PhantomData<fn() -> T>,
}

pub type SqliteKeepStream = SqliteSyncStream<KeepNote>;
pub type SqliteKeepTagStream = SqliteSyncStream<KeepTag>;

impl<T> SqliteSyncStream<T>
where
    T: DeserializeOwned + Serialize + Send + Sync + 'static,
{
    /// Create the stream and initialize the SQLite schema.
    pub fn open(stream: &str, collection: &str, path: PathBuf) -> SyncResult<Self> {
        let stream = SyncStreamName::new(stream)?;
        let collection = SyncCollectionName::new(collection)?;
        open_database(&path)?;

        Ok(Self {
            stream,
            collection,
            path,
            lock: Arc::new(Mutex::new(())),
            _marker: PhantomData,
        })
    }

    pub fn upsert(&self, id: impl Into<String>, row: T) -> SyncResult<SyncCursor> {
        let key = RowKey::new(id.into())?;
        let payload = serde_json::to_value(row)?;
        self.with_transaction(|tx| {
            let version = next_version(tx)?;
            let row = self.write_upsert(tx, key, version, payload)?;
            Ok(row.cursor)
        })
    }

    pub fn reset(&self) -> SyncResult<SyncCursor> {
        self.with_transaction(|tx| {
            let version = next_version(tx)?;
            tx.execute("DELETE FROM keep_rows", [])
                .map_err(sqlite_error)?;
            self.insert_change(tx, version, None, SyncOp::Reset, None)?;
            SyncCursor::new(version.to_string())
        })
    }

    fn with_connection<R>(&self, f: impl FnOnce(&Connection) -> SyncResult<R>) -> SyncResult<R> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| SyncError::backend("keep sqlite stream lock poisoned"))?;
        let conn = open_database(&self.path)?;
        f(&conn)
    }

    fn with_transaction<R>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> SyncResult<R>,
    ) -> SyncResult<R> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| SyncError::backend("keep sqlite stream lock poisoned"))?;
        let mut conn = open_database(&self.path)?;
        let tx = conn.transaction().map_err(sqlite_error)?;
        let result = f(&tx)?;
        tx.commit().map_err(sqlite_error)?;
        Ok(result)
    }

    fn current_cursor_value(&self) -> SyncResult<SyncCursor> {
        self.with_connection(|conn| SyncCursor::new(current_version(conn)?.to_string()))
    }

    fn pull_value(&self, request: SyncPullRequest) -> SyncResult<SyncPullResponse<Value>> {
        self.with_connection(|conn| {
            let cursor = Some(SyncCursor::new(current_version(conn)?.to_string())?);
            let Some(after) = request.cursor else {
                let rows = self.snapshot_rows(conn)?;
                return Ok(SyncPullResponse::snapshot(
                    self.stream.clone(),
                    self.collection.clone(),
                    rows,
                    cursor,
                ));
            };

            let after = after
                .as_str()
                .parse::<u64>()
                .map_err(|_| SyncError::Gap(after.to_string()))?;
            let changes = self.incremental_changes(conn, after)?;
            Ok(SyncPullResponse::incremental(
                self.stream.clone(),
                self.collection.clone(),
                changes,
                cursor,
            ))
        })
    }

    fn push_value(&self, request: SyncPushRequest<Value>) -> SyncResult<SyncPushResponse<Value>> {
        self.with_transaction(|tx| {
            let mut response = SyncPushResponse::new(self.stream.clone());
            response.collection = Some(self.collection.clone());

            for mutation in request.mutations {
                if mutation_was_accepted(tx, mutation.id.as_str())? {
                    response.accepted.push(mutation.id);
                    continue;
                }

                match self.apply_mutation(tx, mutation)? {
                    SqliteMutationOutcome::Accepted {
                        mutation_id,
                        row,
                        cursor,
                    } => {
                        record_accepted_mutation(tx, mutation_id.as_str())?;
                        response.accepted.push(mutation_id);
                        if let Some(row) = row {
                            response.rows.push(row);
                        }
                        response.cursor = Some(cursor);
                    }
                    SqliteMutationOutcome::Rejected(rejected) => {
                        response.rejected.push(rejected);
                    }
                    SqliteMutationOutcome::Conflict(conflict) => {
                        response.conflicts.push(conflict);
                    }
                }
            }

            Ok(response)
        })
    }

    fn apply_mutation(
        &self,
        tx: &Transaction<'_>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<SqliteMutationOutcome> {
        match mutation.op {
            SyncOp::Upsert => self.apply_upsert(tx, mutation),
            SyncOp::Delete => self.apply_delete(tx, mutation),
            SyncOp::Reset => self.apply_reset(tx, mutation),
        }
    }

    fn apply_upsert(
        &self,
        tx: &Transaction<'_>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<SqliteMutationOutcome> {
        let Some(key) = mutation.key.clone() else {
            return Ok(SqliteMutationOutcome::Rejected(SyncRejectedMutation {
                mutation_id: mutation.id,
                key: None,
                reason: "upsert requires a row key".to_string(),
            }));
        };

        if let Some(conflict) = self.version_conflict(tx, &mutation, key.as_str())? {
            return Ok(SqliteMutationOutcome::Conflict(conflict));
        }

        let value = match serde_json::from_value::<T>(mutation.payload.clone()) {
            Ok(value) => value,
            Err(err) => {
                return Ok(SqliteMutationOutcome::Rejected(SyncRejectedMutation {
                    mutation_id: mutation.id,
                    key: Some(key),
                    reason: format!("invalid mutation payload: {err}"),
                }));
            }
        };
        let payload = serde_json::to_value(value)?;
        let version = next_version(tx)?;
        let row = self.write_upsert(tx, key, version, payload)?;

        Ok(SqliteMutationOutcome::Accepted {
            mutation_id: mutation.id,
            row: Some(row.row),
            cursor: row.cursor,
        })
    }

    fn apply_delete(
        &self,
        tx: &Transaction<'_>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<SqliteMutationOutcome> {
        let Some(key) = mutation.key.clone() else {
            return Ok(SqliteMutationOutcome::Rejected(SyncRejectedMutation {
                mutation_id: mutation.id,
                key: None,
                reason: "delete requires a row key".to_string(),
            }));
        };

        if let Some(conflict) = self.version_conflict(tx, &mutation, key.as_str())? {
            return Ok(SqliteMutationOutcome::Conflict(conflict));
        }

        let version = next_version(tx)?;
        tx.execute("DELETE FROM keep_rows WHERE row_key = ?1", [key.as_str()])
            .map_err(sqlite_error)?;
        self.insert_change(tx, version, Some(&key), SyncOp::Delete, None)?;

        Ok(SqliteMutationOutcome::Accepted {
            mutation_id: mutation.id,
            row: None,
            cursor: SyncCursor::new(version.to_string())?,
        })
    }

    fn apply_reset(
        &self,
        tx: &Transaction<'_>,
        mutation: ClientMutation<Value>,
    ) -> SyncResult<SqliteMutationOutcome> {
        let version = next_version(tx)?;
        tx.execute("DELETE FROM keep_rows", [])
            .map_err(sqlite_error)?;
        self.insert_change(tx, version, None, SyncOp::Reset, None)?;

        Ok(SqliteMutationOutcome::Accepted {
            mutation_id: mutation.id,
            row: None,
            cursor: SyncCursor::new(version.to_string())?,
        })
    }

    fn write_upsert(
        &self,
        tx: &Transaction<'_>,
        key: RowKey,
        version: u64,
        payload: Value,
    ) -> SyncResult<AppliedRow> {
        let payload = serde_json::to_string(&payload)?;
        tx.execute(
            r#"
            INSERT INTO keep_rows (row_key, version, payload)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(row_key) DO UPDATE SET
                version = excluded.version,
                payload = excluded.payload
            "#,
            params![key.as_str(), version_to_i64(version)?, payload],
        )
        .map_err(sqlite_error)?;

        let row = sync_row_from_parts(key.as_str().to_string(), version, payload)?;
        self.insert_change(tx, version, Some(&key), SyncOp::Upsert, Some(&row))?;
        Ok(AppliedRow {
            row,
            cursor: SyncCursor::new(version.to_string())?,
        })
    }

    fn version_conflict(
        &self,
        tx: &Transaction<'_>,
        mutation: &ClientMutation<Value>,
        key: &str,
    ) -> SyncResult<Option<SyncConflict<Value>>> {
        let Some(expected) = mutation.base_version.as_ref() else {
            return Ok(None);
        };
        let server_row = select_row(tx, key)?.map(sync_row_from_stored).transpose()?;
        let server_version = server_row.as_ref().and_then(|row| row.version.as_ref());
        if server_version == Some(expected) {
            return Ok(None);
        }

        Ok(Some(SyncConflict {
            mutation_id: mutation.id.clone(),
            key: mutation.key.clone(),
            server_row,
            reason: "base version is stale".to_string(),
        }))
    }

    fn snapshot_rows(&self, conn: &Connection) -> SyncResult<Vec<SyncRow<Value>>> {
        let mut stmt = conn
            .prepare("SELECT row_key, version, payload FROM keep_rows ORDER BY row_key")
            .map_err(sqlite_error)?;
        let rows = stmt
            .query_map([], stored_row_from_sql)
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;

        rows.into_iter().map(sync_row_from_stored).collect()
    }

    fn incremental_changes(
        &self,
        conn: &Connection,
        after: u64,
    ) -> SyncResult<Vec<SyncChange<Value>>> {
        let mut stmt = conn
            .prepare(
                r#"
                SELECT cursor, row_key, op, row_version, payload
                FROM keep_changes
                WHERE cursor > ?1
                ORDER BY cursor
                "#,
            )
            .map_err(sqlite_error)?;
        let changes = stmt
            .query_map([version_to_i64(after)?], stored_change_from_sql)
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;

        changes
            .into_iter()
            .map(|change| self.sync_change_from_stored(change))
            .collect()
    }

    fn insert_change(
        &self,
        tx: &Transaction<'_>,
        cursor: u64,
        key: Option<&RowKey>,
        op: SyncOp,
        row: Option<&SyncRow<Value>>,
    ) -> SyncResult<()> {
        let payload = row
            .map(|row| serde_json::to_string(&row.value))
            .transpose()?;
        let row_version = row
            .and_then(|row| row.version.as_ref())
            .map(|version| version.as_str().to_string());
        tx.execute(
            r#"
            INSERT INTO keep_changes (cursor, row_key, op, row_version, payload)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                version_to_i64(cursor)?,
                key.map(RowKey::as_str),
                op_to_db(op),
                row_version,
                payload
            ],
        )
        .map_err(sqlite_error)?;
        Ok(())
    }

    fn sync_change_from_stored(&self, change: StoredChange) -> SyncResult<SyncChange<Value>> {
        let op = op_from_db(&change.op)?;
        let key_value = change.key.clone();
        let key = key_value.clone().map(RowKey::new).transpose()?;
        let row = match (key_value.as_deref(), change.row_version, change.payload) {
            (Some(key), Some(version), Some(payload)) => Some(sync_row_from_parts(
                key.to_string(),
                parse_row_version(&version)?,
                payload,
            )?),
            _ => None,
        };

        Ok(SyncChange {
            stream: self.stream.clone(),
            collection: self.collection.clone(),
            key,
            op,
            row,
            cursor: SyncCursor::new(change.cursor.to_string())?,
        })
    }
}

impl<T> SyncStreamSource for SqliteSyncStream<T>
where
    T: DeserializeOwned + Serialize + Send + Sync + 'static,
{
    fn stream(&self) -> &SyncStreamName {
        &self.stream
    }

    fn collection(&self) -> &SyncCollectionName {
        &self.collection
    }

    fn current_cursor(&self, ctx: &RequestContext) -> Option<SyncCursor> {
        let _ = ctx;
        self.current_cursor_value().ok()
    }

    fn pull<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPullRequest,
    ) -> SyncBoxFuture<'a, SyncPullResponse<Value>> {
        let _ = ctx;
        Box::pin(async move { self.pull_value(request) })
    }

    fn push<'a>(
        &'a self,
        ctx: RequestContext,
        request: SyncPushRequest<Value>,
    ) -> SyncBoxFuture<'a, SyncPushResponse<Value>> {
        let _ = ctx;
        Box::pin(async move { self.push_value(request) })
    }
}

struct AppliedRow {
    row: SyncRow<Value>,
    cursor: SyncCursor,
}

enum SqliteMutationOutcome {
    Accepted {
        mutation_id: pocopine_sync::MutationId,
        row: Option<SyncRow<Value>>,
        cursor: SyncCursor,
    },
    Rejected(SyncRejectedMutation),
    Conflict(SyncConflict<Value>),
}

struct StoredRow {
    key: String,
    version: u64,
    payload: String,
}

struct StoredChange {
    cursor: u64,
    key: Option<String>,
    op: String,
    row_version: Option<String>,
    payload: Option<String>,
}

fn open_database(path: &Path) -> SyncResult<Connection> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|err| SyncError::backend(err.to_string()))?;
    }
    let conn = Connection::open(path).map_err(sqlite_error)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(sqlite_error)?;
    bootstrap_schema(&conn)?;
    Ok(conn)
}

fn bootstrap_schema(conn: &Connection) -> SyncResult<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS keep_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )
    .map_err(sqlite_error)?;

    let existing: Option<String> = conn
        .query_row(
            "SELECT value FROM keep_meta WHERE key = ?1",
            [META_SCHEMA_VERSION],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;

    match existing.as_deref() {
        None => {
            create_stream_tables(conn)?;
            set_meta(conn, META_SCHEMA_VERSION, SCHEMA_VERSION)?;
            set_meta(conn, META_CURSOR_VERSION, "0")?;
        }
        Some(LEGACY_SCHEMA_VERSION) => {
            create_stream_tables(conn)?;
            ensure_row_version_column(conn)?;
            set_meta(conn, META_SCHEMA_VERSION, SCHEMA_VERSION)?;
            set_meta(conn, META_CURSOR_VERSION, "0")?;
        }
        Some(SCHEMA_VERSION) => {
            create_stream_tables(conn)?;
            ensure_row_version_column(conn)?;
            conn.execute(
                "INSERT OR IGNORE INTO keep_meta (key, value) VALUES (?1, ?2)",
                [META_CURSOR_VERSION, "0"],
            )
            .map_err(sqlite_error)?;
        }
        Some(other) => {
            return Err(SyncError::backend(format!(
                "incompatible keep sqlite schema version: {other}"
            )));
        }
    }

    Ok(())
}

fn create_stream_tables(conn: &Connection) -> SyncResult<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS keep_rows (
            row_key TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            payload TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS keep_changes (
            cursor INTEGER PRIMARY KEY,
            row_key TEXT,
            op TEXT NOT NULL,
            row_version TEXT,
            payload TEXT
        );
        CREATE TABLE IF NOT EXISTS keep_accepted_mutations (
            mutation_id TEXT PRIMARY KEY
        );
        "#,
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn ensure_row_version_column(conn: &Connection) -> SyncResult<()> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(keep_rows)")
        .map_err(sqlite_error)?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    if !columns.iter().any(|column| column == "version") {
        conn.execute(
            "ALTER TABLE keep_rows ADD COLUMN version INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .map_err(sqlite_error)?;
    }
    Ok(())
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> SyncResult<()> {
    conn.execute(
        r#"
        INSERT INTO keep_meta (key, value) VALUES (?1, ?2)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        "#,
        [key, value],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn current_version(conn: &Connection) -> SyncResult<u64> {
    let value: String = conn
        .query_row(
            "SELECT value FROM keep_meta WHERE key = ?1",
            [META_CURSOR_VERSION],
            |row| row.get(0),
        )
        .map_err(sqlite_error)?;
    value
        .parse::<u64>()
        .map_err(|_| SyncError::backend("keep sqlite cursor version is not numeric"))
}

fn next_version(tx: &Transaction<'_>) -> SyncResult<u64> {
    let current = current_version(tx)?;
    let next = current.saturating_add(1);
    tx.execute(
        "UPDATE keep_meta SET value = ?1 WHERE key = ?2",
        params![next.to_string(), META_CURSOR_VERSION],
    )
    .map_err(sqlite_error)?;
    Ok(next)
}

fn mutation_was_accepted(tx: &Transaction<'_>, mutation_id: &str) -> SyncResult<bool> {
    let seen: Option<String> = tx
        .query_row(
            "SELECT mutation_id FROM keep_accepted_mutations WHERE mutation_id = ?1",
            [mutation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(seen.is_some())
}

fn record_accepted_mutation(tx: &Transaction<'_>, mutation_id: &str) -> SyncResult<()> {
    tx.execute(
        "INSERT INTO keep_accepted_mutations (mutation_id) VALUES (?1)",
        [mutation_id],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn select_row(conn: &Connection, key: &str) -> SyncResult<Option<StoredRow>> {
    conn.query_row(
        "SELECT row_key, version, payload FROM keep_rows WHERE row_key = ?1",
        [key],
        stored_row_from_sql,
    )
    .optional()
    .map_err(sqlite_error)
}

fn stored_row_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok(StoredRow {
        key: row.get(0)?,
        version: i64_to_version(row.get::<_, i64>(1)?)?,
        payload: row.get(2)?,
    })
}

fn stored_change_from_sql(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredChange> {
    Ok(StoredChange {
        cursor: i64_to_version(row.get::<_, i64>(0)?)?,
        key: row.get(1)?,
        op: row.get(2)?,
        row_version: row.get(3)?,
        payload: row.get(4)?,
    })
}

fn sync_row_from_stored(row: StoredRow) -> SyncResult<SyncRow<Value>> {
    sync_row_from_parts(row.key, row.version, row.payload)
}

fn sync_row_from_parts(key: String, version: u64, payload: String) -> SyncResult<SyncRow<Value>> {
    Ok(SyncRow {
        key: RowKey::new(key)?,
        value: serde_json::from_str(&payload)?,
        version: Some(RowVersion::new(format!("v{version}"))?),
        pending: false,
        conflict: false,
    })
}

fn parse_row_version(version: &str) -> SyncResult<u64> {
    version
        .strip_prefix('v')
        .ok_or_else(|| SyncError::backend("keep sqlite row version is missing v prefix"))?
        .parse::<u64>()
        .map_err(|_| SyncError::backend("keep sqlite row version is not numeric"))
}

fn op_to_db(op: SyncOp) -> &'static str {
    match op {
        SyncOp::Upsert => "upsert",
        SyncOp::Delete => "delete",
        SyncOp::Reset => "reset",
    }
}

fn op_from_db(op: &str) -> SyncResult<SyncOp> {
    match op {
        "upsert" => Ok(SyncOp::Upsert),
        "delete" => Ok(SyncOp::Delete),
        "reset" => Ok(SyncOp::Reset),
        _ => Err(SyncError::backend(format!(
            "unknown keep sqlite sync op: {op}"
        ))),
    }
}

fn version_to_i64(version: u64) -> SyncResult<i64> {
    i64::try_from(version).map_err(|_| SyncError::backend("keep sqlite version overflow"))
}

fn i64_to_version(version: i64) -> rusqlite::Result<u64> {
    u64::try_from(version).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Integer, Box::new(err))
    })
}

fn sqlite_error(err: rusqlite::Error) -> SyncError {
    SyncError::backend(format!("keep sqlite error: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_server::axum::http::{HeaderMap, Method, Uri};
    use pocopine_sync::{
        ClientMutation, MutationId, SyncPullMode, SyncPullRequest, SyncPushRequest,
    };

    fn test_note(id: &str, title: &str) -> KeepNote {
        KeepNote {
            id: id.to_string(),
            title: title.to_string(),
            body: String::new(),
            body_state: None,
            color: "default".to_string(),
            pinned: false,
            archived: false,
            todos: Vec::new(),
            labels: Vec::new(),
            updated_at_ms: 0,
        }
    }

    fn test_context() -> RequestContext {
        RequestContext::new(Method::GET, Uri::from_static("/"), HeaderMap::new())
    }

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pocopine_keep_{name}_{}_{}.sqlite3",
            std::process::id(),
            crate::now_ms()
        ))
    }

    #[tokio::test]
    async fn sqlite_stream_rehydrates_persisted_rows() {
        let path = test_path("rehydrates");
        let _ = std::fs::remove_file(&path);

        {
            let stream =
                SqliteKeepStream::open(crate::KEEP_STREAM, crate::KEEP_COLLECTION, path.clone())
                    .unwrap();
            stream
                .upsert("note_1", test_note("note_1", "Persisted"))
                .unwrap();
        }

        let stream =
            SqliteKeepStream::open(crate::KEEP_STREAM, crate::KEEP_COLLECTION, path.clone())
                .unwrap();
        let request = SyncPullRequest::new(SyncStreamName::new(crate::KEEP_STREAM).unwrap());
        let response = stream.pull(test_context(), request).await.unwrap();

        assert_eq!(response.mode, SyncPullMode::Snapshot);
        assert_eq!(response.rows.len(), 1);
        assert_eq!(response.rows[0].value["id"], "note_1");
        assert_eq!(response.rows[0].value["title"], "Persisted");

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn sqlite_stream_persists_incremental_changes() {
        let path = test_path("incremental");
        let _ = std::fs::remove_file(&path);

        {
            let stream =
                SqliteKeepStream::open(crate::KEEP_STREAM, crate::KEEP_COLLECTION, path.clone())
                    .unwrap();
            stream
                .upsert("note_1", test_note("note_1", "First"))
                .unwrap();
        }

        let stream =
            SqliteKeepStream::open(crate::KEEP_STREAM, crate::KEEP_COLLECTION, path.clone())
                .unwrap();
        let response = stream
            .pull(
                test_context(),
                SyncPullRequest::new(SyncStreamName::new(crate::KEEP_STREAM).unwrap())
                    .cursor(Some(SyncCursor::new("0").unwrap())),
            )
            .await
            .unwrap();

        assert_eq!(response.mode, SyncPullMode::Incremental);
        assert_eq!(response.changes.len(), 1);
        assert_eq!(response.changes[0].op, SyncOp::Upsert);
        assert_eq!(
            response.changes[0].row.as_ref().unwrap().value["title"],
            "First"
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn sqlite_stream_dedupes_accepted_mutations_in_sqlite() {
        let path = test_path("dedupe");
        let _ = std::fs::remove_file(&path);

        let stream =
            SqliteKeepStream::open(crate::KEEP_STREAM, crate::KEEP_COLLECTION, path.clone())
                .unwrap();
        let mutation = ClientMutation {
            id: MutationId::new("device_1:dupe").unwrap(),
            key: Some(RowKey::new("note_1").unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: serde_json::to_value(test_note("note_1", "First")).unwrap(),
        };

        stream
            .push(
                test_context(),
                SyncPushRequest::new(
                    SyncStreamName::new(crate::KEEP_STREAM).unwrap(),
                    [mutation.clone()],
                ),
            )
            .await
            .unwrap();
        stream
            .upsert("note_1", test_note("note_1", "Server replacement"))
            .unwrap();

        let stream =
            SqliteKeepStream::open(crate::KEEP_STREAM, crate::KEEP_COLLECTION, path.clone())
                .unwrap();
        let replayed = stream
            .push(
                test_context(),
                SyncPushRequest::new(SyncStreamName::new(crate::KEEP_STREAM).unwrap(), [mutation]),
            )
            .await
            .unwrap();

        assert_eq!(replayed.accepted[0].as_str(), "device_1:dupe");
        assert!(replayed.cursor.is_none());

        let response = stream
            .pull(
                test_context(),
                SyncPullRequest::new(SyncStreamName::new(crate::KEEP_STREAM).unwrap()),
            )
            .await
            .unwrap();

        assert_eq!(response.rows.len(), 1);
        assert_eq!(response.rows[0].value["title"], "Server replacement");

        let _ = std::fs::remove_file(path);
    }
}
