//! SQLite-backed sync-query [`Source`] for the Keep example server.
//!
//! This is the server-side authority for the example app, ported from
//! the legacy `SyncStreamSource` change-log model to RFC-090's
//! [`Source`] trait. The `Source` pull is **snapshot-only**, so this
//! store is much simpler than the old one: it persists rows
//! (`keep_rows`) and an idempotency log (`keep_accepted_mutations`) —
//! no incremental `keep_changes` table and no server cursor counter.
//!
//! Optimistic concurrency reuses the row's own `updated_at_ms` as the
//! version token (every Keep edit already bumps it), exposed to the
//! framework via `SourceResource::version_field`.

use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pocopine_server::RequestContext;
use pocopine_sync::{MutationId, RowKey, RowVersion, SyncError, SyncOp, SyncResult};
use pocopine_sync_query::source::{Source, SourceFuture, SourceStream};
use pocopine_sync_query::write::{
    AcceptedMutation, Conflict, DeleteResult, MutationLog, MutationReservation, WriteResult,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{KeepNote, KeepNoteDraft, KeepTag, KeepTagDraft};

const SCHEMA_VERSION: &str = "3";
const META_SCHEMA_VERSION: &str = "schema_version";

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

/// Bridge a Keep domain row to the bits the generic SQLite source
/// needs: its key, its optimistic-concurrency token, and how to build
/// one from `(id, draft)`.
pub trait KeepRow: Clone + Serialize + DeserializeOwned + Send + Sync + 'static {
    type Draft: Clone + Serialize + DeserializeOwned + Send + Sync + 'static;

    /// The row's stable id (also its `RowKey`).
    fn row_id(&self) -> String;
    /// Optimistic-concurrency token — Keep reuses `updated_at_ms`.
    fn version_token(&self) -> String;
    /// Build the canonical row from a create/update draft.
    fn build(id: String, draft: Self::Draft) -> Self;
}

impl KeepRow for KeepNote {
    type Draft = KeepNoteDraft;

    fn row_id(&self) -> String {
        self.id.clone()
    }
    fn version_token(&self) -> String {
        self.updated_at_ms.to_string()
    }
    fn build(id: String, draft: Self::Draft) -> Self {
        KeepNote::from((id, draft))
    }
}

impl KeepRow for KeepTag {
    type Draft = KeepTagDraft;

    fn row_id(&self) -> String {
        self.id.clone()
    }
    fn version_token(&self) -> String {
        self.updated_at_ms.to_string()
    }
    fn build(id: String, draft: Self::Draft) -> Self {
        KeepTag::from((id, draft))
    }
}

/// Optimistic-concurrency version extractor for [`KeepRow`] streams,
/// passed to `SourceResource::version_field`.
pub fn keep_row_version<R: KeepRow>(row: &R) -> SyncResult<Option<RowVersion>> {
    Ok(Some(RowVersion::new(row.version_token())?))
}

/// Generic SQLite-backed [`Source`] + [`MutationLog`] for a Keep row.
#[derive(Clone)]
pub struct SqliteKeepSource<R: KeepRow> {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
    _marker: PhantomData<fn() -> R>,
}

pub type KeepNoteSource = SqliteKeepSource<KeepNote>;
pub type KeepTagSource = SqliteKeepSource<KeepTag>;

impl<R: KeepRow> SqliteKeepSource<R> {
    /// Open the store and initialize the SQLite schema.
    pub fn open(path: PathBuf) -> SyncResult<Self> {
        open_database(&path)?;
        Ok(Self {
            path,
            lock: Arc::new(Mutex::new(())),
            _marker: PhantomData,
        })
    }

    /// Delete every row (used by the `reset_keep_notes` server fn).
    /// The accepted-mutation log is cleared too so a fresh demo run
    /// starts from a clean slate.
    pub fn reset(&self) -> SyncResult<()> {
        self.with_transaction(|tx| {
            tx.execute("DELETE FROM keep_rows", [])
                .map_err(sqlite_error)?;
            tx.execute("DELETE FROM keep_accepted_mutations", [])
                .map_err(sqlite_error)?;
            Ok(())
        })
    }

    fn with_connection<T>(&self, f: impl FnOnce(&Connection) -> SyncResult<T>) -> SyncResult<T> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| SyncError::backend("keep sqlite source lock poisoned"))?;
        let conn = open_database(&self.path)?;
        f(&conn)
    }

    fn with_transaction<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> SyncResult<T>,
    ) -> SyncResult<T> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| SyncError::backend("keep sqlite source lock poisoned"))?;
        let mut conn = open_database(&self.path)?;
        let tx = conn.transaction().map_err(sqlite_error)?;
        let result = f(&tx)?;
        tx.commit().map_err(sqlite_error)?;
        Ok(result)
    }

    fn load_all(&self) -> SyncResult<Vec<R>> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("SELECT payload FROM keep_rows ORDER BY row_key")
                .map_err(sqlite_error)?;
            let payloads = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?;
            payloads
                .into_iter()
                .map(|payload| serde_json::from_str::<R>(&payload).map_err(SyncError::from))
                .collect()
        })
    }

    fn load_one(&self, id: &str) -> SyncResult<Option<R>> {
        self.with_connection(|conn| select_row::<R>(conn, id))
    }

    fn put(&self, row: &R) -> SyncResult<()> {
        let key = row.row_id();
        let payload = serde_json::to_string(row)?;
        self.with_transaction(|tx| {
            tx.execute(
                r#"
                INSERT INTO keep_rows (row_key, payload)
                VALUES (?1, ?2)
                ON CONFLICT(row_key) DO UPDATE SET payload = excluded.payload
                "#,
                params![key, payload],
            )
            .map_err(sqlite_error)?;
            Ok(())
        })
    }

    fn remove(&self, id: &str) -> SyncResult<()> {
        self.with_transaction(|tx| {
            tx.execute("DELETE FROM keep_rows WHERE row_key = ?1", [id])
                .map_err(sqlite_error)?;
            Ok(())
        })
    }
}

impl<R: KeepRow> Source for SqliteKeepSource<R> {
    type Id = String;
    type Row = R;
    type Draft = R::Draft;
    type Context = ();

    fn extract_context<'a>(
        &'a self,
        _ctx: RequestContext,
    ) -> SourceFuture<'a, SyncResult<Self::Context>> {
        Box::pin(async { Ok(()) })
    }

    fn list_stream<'a>(
        &'a self,
        _ctx: Self::Context,
        _query: &'a pocopine_sync_query::Query<Self::Row>,
    ) -> SourceStream<'a, Self::Row> {
        // Keep is single-user per stream; serve the whole snapshot and
        // let the client predicate-filter. The framework still clamps
        // to the query's limit.
        let rows = self.load_all();
        Box::pin(async_stream_from(rows))
    }

    fn get<'a>(
        &'a self,
        _ctx: Self::Context,
        id: Self::Id,
    ) -> SourceFuture<'a, SyncResult<Option<Self::Row>>> {
        Box::pin(async move { self.load_one(&id) })
    }

    fn create<'a>(
        &'a self,
        _ctx: Self::Context,
        _meta: pocopine_sync_query::WriteMeta,
        id: Self::Id,
        draft: Self::Draft,
    ) -> SourceFuture<'a, SyncResult<Self::Row>> {
        Box::pin(async move {
            let row = R::build(id, draft);
            self.put(&row)?;
            Ok(row)
        })
    }

    fn update<'a>(
        &'a self,
        _ctx: Self::Context,
        _meta: pocopine_sync_query::WriteMeta,
        id: Self::Id,
        draft: Self::Draft,
        expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<WriteResult<Self::Row>>> {
        Box::pin(async move {
            if let Some(expected) = expected_version.as_ref() {
                match self.load_one(&id)? {
                    None => {
                        return Ok(WriteResult::Conflict(Conflict::new(
                            None,
                            "row was deleted on the server",
                        )));
                    }
                    Some(stored) if stored.version_token() != expected.as_str() => {
                        return Ok(WriteResult::Conflict(Conflict::stale(Some(stored))));
                    }
                    Some(_) => {}
                }
            }
            let row = R::build(id, draft);
            self.put(&row)?;
            Ok(WriteResult::Applied(row))
        })
    }

    fn delete<'a>(
        &'a self,
        _ctx: Self::Context,
        _meta: pocopine_sync_query::WriteMeta,
        id: Self::Id,
        expected_version: Option<RowVersion>,
    ) -> SourceFuture<'a, SyncResult<DeleteResult<Self::Row>>> {
        Box::pin(async move {
            if let Some(expected) = expected_version.as_ref() {
                match self.load_one(&id)? {
                    // Already gone — deletion is idempotent.
                    None => return Ok(DeleteResult::Applied),
                    Some(stored) if stored.version_token() != expected.as_str() => {
                        return Ok(DeleteResult::Conflict(Conflict::stale(Some(stored))));
                    }
                    Some(_) => {}
                }
            }
            self.remove(&id)?;
            Ok(DeleteResult::Applied)
        })
    }

    fn mutation_log(&self) -> Option<Arc<dyn MutationLog<Self::Row>>> {
        Some(Arc::new(self.clone()))
    }
}

#[async_trait::async_trait]
impl<R: KeepRow> MutationLog<R> for SqliteKeepSource<R> {
    async fn reserve_mutation(
        &self,
        _ctx: &RequestContext,
        candidate: AcceptedMutation,
    ) -> SyncResult<MutationReservation> {
        // Keep is single-user, so the idempotency scope is global.
        self.with_transaction(|tx| {
            let payload = serde_json::to_string(&candidate.payload)?;
            let inserted = tx
                .execute(
                    r#"
                    INSERT OR IGNORE INTO keep_accepted_mutations
                        (mutation_id, op, row_key, payload)
                    VALUES (?1, ?2, ?3, ?4)
                    "#,
                    params![
                        candidate.mutation_id.as_str(),
                        op_to_db(candidate.op),
                        candidate.key.as_ref().map(RowKey::as_str),
                        payload,
                    ],
                )
                .map_err(sqlite_error)?;
            if inserted == 1 {
                return Ok(MutationReservation::Reserved);
            }
            let existing = select_accepted_mutation(tx, candidate.mutation_id.as_str())?
                .ok_or_else(|| SyncError::backend("accepted mutation vanished after conflict"))?;
            Ok(MutationReservation::AlreadyAccepted(existing))
        })
    }

    async fn accepted_mutation(
        &self,
        _ctx: &RequestContext,
        mutation_id: &MutationId,
    ) -> SyncResult<Option<AcceptedMutation>> {
        self.with_connection(|conn| select_accepted_mutation(conn, mutation_id.as_str()))
    }
}

/// Wrap an eagerly-loaded `Vec<R>` result as a `Source` row stream.
/// A load error surfaces as the stream's single `Err` item.
fn async_stream_from<R: Send + 'static>(
    rows: SyncResult<Vec<R>>,
) -> impl futures::stream::Stream<Item = SyncResult<R>> + Send + 'static {
    let items: Vec<SyncResult<R>> = match rows {
        Ok(rows) => rows.into_iter().map(Ok).collect(),
        Err(err) => vec![Err(err)],
    };
    futures::stream::iter(items)
}

fn select_row<R: DeserializeOwned>(conn: &Connection, id: &str) -> SyncResult<Option<R>> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload FROM keep_rows WHERE row_key = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    payload
        .map(|payload| serde_json::from_str::<R>(&payload).map_err(SyncError::from))
        .transpose()
}

fn select_accepted_mutation(
    conn: &Connection,
    mutation_id: &str,
) -> SyncResult<Option<AcceptedMutation>> {
    let row: Option<(String, Option<String>, String)> = conn
        .query_row(
            "SELECT op, row_key, payload FROM keep_accepted_mutations WHERE mutation_id = ?1",
            [mutation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((op, row_key, payload)) = row else {
        return Ok(None);
    };
    Ok(Some(AcceptedMutation::new(
        MutationId::new(mutation_id.to_string())?,
        op_from_db(&op)?,
        row_key.map(RowKey::new).transpose()?,
        serde_json::from_str(&payload)?,
    )))
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
        CREATE TABLE IF NOT EXISTS keep_rows (
            row_key TEXT PRIMARY KEY,
            payload TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS keep_accepted_mutations (
            mutation_id TEXT PRIMARY KEY,
            op TEXT NOT NULL,
            row_key TEXT,
            payload TEXT NOT NULL
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
        Some(SCHEMA_VERSION) => {}
        // Any older/legacy change-log schema: the Source model is a
        // clean break, so drop the legacy tables and start fresh.
        // (Demo data only — no migration path is warranted.)
        _ => {
            conn.execute_batch(
                r#"
                DROP TABLE IF EXISTS keep_changes;
                DELETE FROM keep_rows;
                DELETE FROM keep_accepted_mutations;
                "#,
            )
            .map_err(sqlite_error)?;
            set_meta(conn, META_SCHEMA_VERSION, SCHEMA_VERSION)?;
        }
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
        other => Err(SyncError::backend(format!(
            "unknown keep sqlite sync op: {other}"
        ))),
    }
}

fn sqlite_error(err: rusqlite::Error) -> SyncError {
    SyncError::backend(format!("keep sqlite error: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_server::axum::http::{HeaderMap, Method, Uri};
    use pocopine_sync::SyncStreamName;
    use pocopine_sync_query::Query;

    fn test_meta(n: u64) -> pocopine_sync_query::WriteMeta {
        pocopine_sync_query::WriteMeta {
            mutation_id: pocopine_sync::MutationId::new(format!("device_test:{n}")).unwrap(),
        }
    }

    fn note_draft(title: &str, updated_at_ms: u64) -> KeepNoteDraft {
        KeepNoteDraft {
            title: title.to_string(),
            updated_at_ms,
            ..Default::default()
        }
    }

    fn ctx() -> RequestContext {
        RequestContext::new(Method::POST, Uri::from_static("/"), HeaderMap::new())
    }

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "pocopine_keep_src_{name}_{}_{}.sqlite3",
            std::process::id(),
            crate::now_ms()
        ))
    }

    async fn collect(source: &KeepNoteSource) -> Vec<KeepNote> {
        use futures::stream::StreamExt;
        let query: Query<KeepNote> =
            Query::builder(SyncStreamName::new(crate::KEEP_STREAM).unwrap()).build();
        source
            .list_stream((), &query)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<SyncResult<Vec<_>>>()
            .unwrap()
    }

    #[tokio::test]
    async fn create_then_list_round_trips() {
        let path = test_path("create");
        let _ = std::fs::remove_file(&path);
        let source = KeepNoteSource::open(path.clone()).unwrap();

        let row = source
            .create(
                (),
                test_meta(1),
                "note_1".to_string(),
                note_draft("Hello", 10),
            )
            .await
            .unwrap();
        assert_eq!(row.title, "Hello");

        let rows = collect(&source).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "note_1");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn update_detects_stale_version() {
        let path = test_path("stale");
        let _ = std::fs::remove_file(&path);
        let source = KeepNoteSource::open(path.clone()).unwrap();

        source
            .create((), test_meta(2), "n".to_string(), note_draft("v10", 10))
            .await
            .unwrap();

        // Stale expected_version → conflict carrying the server row.
        let result = source
            .update(
                (),
                test_meta(3),
                "n".to_string(),
                note_draft("v99", 99),
                Some(RowVersion::new("5").unwrap()),
            )
            .await
            .unwrap();
        match result {
            WriteResult::Conflict(c) => {
                assert_eq!(c.server_row.unwrap().updated_at_ms, 10);
            }
            WriteResult::Applied(_) => panic!("expected conflict"),
        }

        // Correct expected_version → applied.
        let ok = source
            .update(
                (),
                test_meta(4),
                "n".to_string(),
                note_draft("v20", 20),
                Some(RowVersion::new("10").unwrap()),
            )
            .await
            .unwrap();
        assert!(matches!(ok, WriteResult::Applied(row) if row.updated_at_ms == 20));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn mutation_log_reserves_then_dedupes() {
        let path = test_path("dedupe");
        let _ = std::fs::remove_file(&path);
        let source = KeepNoteSource::open(path.clone()).unwrap();

        let candidate = AcceptedMutation::new(
            MutationId::new("device_1:1").unwrap(),
            SyncOp::Upsert,
            Some(RowKey::new("note_1").unwrap()),
            serde_json::json!({"op": "create", "id": "note_1"}),
        );

        let first = source
            .reserve_mutation(&ctx(), candidate.clone())
            .await
            .unwrap();
        assert!(matches!(first, MutationReservation::Reserved));

        let replay = source
            .reserve_mutation(&ctx(), candidate.clone())
            .await
            .unwrap();
        match replay {
            MutationReservation::AlreadyAccepted(prior) => assert_eq!(prior, candidate),
            MutationReservation::Reserved => panic!("expected dedupe"),
        }
        let _ = std::fs::remove_file(path);
    }
}
