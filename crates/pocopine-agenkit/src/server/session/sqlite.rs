//! An indexed, durable [`SessionStore`] backed by SQLite (roadmap W7).
//!
//! Two tables — `threads` (one row per thread; the parent pointer is indexed)
//! and `records` (the append-only log, `PRIMARY KEY (thread_id, seq)`) — so the
//! lookups the JSONL store scans for (`children`, `last_seq`) become indexed
//! queries. `append` is one `INSERT` in a transaction; `delete_thread` is a
//! `DELETE` (a real hard delete — §D10-clean).
//!
//! SQLite is synchronous; like `pocopine-sync-sqlite`'s store we run each query
//! under a `Mutex<Connection>` and return a ready future (the session is
//! single-writer, and the queries are small). Uses the bundled SQLite, so there
//! is no system dependency.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::{
    ParentLink, Record, RecordKind, SessionError, SessionFuture, SessionResult, SessionStore,
    ThreadId, ThreadMeta, now_ms,
};

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS threads (
        id          TEXT PRIMARY KEY,
        parent      TEXT,
        fork_at_seq INTEGER,
        created_ms  INTEGER NOT NULL,
        attributes  TEXT NOT NULL DEFAULT 'null',
        inherits    INTEGER NOT NULL DEFAULT 1
    );
    CREATE INDEX IF NOT EXISTS threads_parent ON threads(parent);
    CREATE TABLE IF NOT EXISTS records (
        thread_id TEXT NOT NULL,
        seq       INTEGER NOT NULL,
        ts_ms     INTEGER NOT NULL,
        kind      TEXT NOT NULL,
        data      TEXT NOT NULL,
        PRIMARY KEY (thread_id, seq)
    );
";

/// A SQLite-backed durable session store.
#[derive(Clone)]
pub struct SqliteSessionStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteSessionStore {
    /// Open (or create) a store at `path`.
    pub fn open(path: impl AsRef<Path>) -> SessionResult<Self> {
        let conn = Connection::open(path).map_err(db_err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(db_err)?;
        Self::from_connection(conn)
    }

    /// An in-memory store (tests).
    pub fn open_in_memory() -> SessionResult<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(db_err)?)
    }

    /// Build from an existing connection, bootstrapping the schema.
    pub fn from_connection(conn: Connection) -> SessionResult<Self> {
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        Self::add_inherits_column(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Add the `inherits` column to a database created before spawn edges
    /// existed. Every link such a database holds is a fork, which is the
    /// column's default, so no backfill is needed. `CREATE TABLE IF NOT EXISTS`
    /// leaves an existing table untouched, so this has to be its own statement.
    fn add_inherits_column(conn: &Connection) -> SessionResult<()> {
        let present = conn
            .prepare("SELECT 1 FROM pragma_table_info('threads') WHERE name = 'inherits'")
            .map_err(db_err)?
            .exists([])
            .map_err(db_err)?;
        if present {
            return Ok(());
        }
        match conn.execute(
            "ALTER TABLE threads ADD COLUMN inherits INTEGER NOT NULL DEFAULT 1",
            [],
        ) {
            Ok(_) => Ok(()),
            // Two processes can both see the column as absent before either
            // ALTER commits. Losing that race means the column now exists, which
            // is the outcome we wanted — not a reason to refuse to start.
            Err(err) if is_duplicate_column(&err) => Ok(()),
            Err(err) => Err(db_err(err)),
        }
    }

    /// Run `f` against the locked connection.
    fn with_conn<T>(
        &self,
        f: impl FnOnce(&mut Connection) -> SessionResult<T>,
    ) -> SessionResult<T> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|_| SessionError::Backend("sqlite session store lock poisoned".into()))?;
        f(&mut conn)
    }

    /// Wrap a synchronously-computed result as a ready future.
    fn ready<T: Send + 'static>(result: SessionResult<T>) -> SessionFuture<'static, T> {
        Box::pin(async move { result })
    }
}

fn db_err(err: rusqlite::Error) -> SessionError {
    SessionError::Backend(err.to_string())
}

/// Whether a failed `ALTER TABLE ... ADD COLUMN` lost a race to another process
/// that added the same column. SQLite reports this as a generic error with a
/// message rather than a distinct code, so the message is what there is to match.
fn is_duplicate_column(err: &rusqlite::Error) -> bool {
    err.to_string().contains("duplicate column name")
}

fn json_err(err: serde_json::Error) -> SessionError {
    SessionError::Corrupt(err.to_string())
}

fn kind_to_str(kind: RecordKind) -> &'static str {
    match kind {
        RecordKind::Message => "message",
        RecordKind::StateChange => "state_change",
        RecordKind::Checkpoint => "checkpoint",
    }
}

fn kind_from_str(s: &str) -> SessionResult<RecordKind> {
    match s {
        "message" => Ok(RecordKind::Message),
        "state_change" => Ok(RecordKind::StateChange),
        "checkpoint" => Ok(RecordKind::Checkpoint),
        other => Err(SessionError::Corrupt(format!(
            "unknown record kind: {other}"
        ))),
    }
}

fn thread_exists(conn: &Connection, id: &ThreadId) -> SessionResult<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM threads WHERE id = ?1",
            params![id.as_str()],
            |_| Ok(()),
        )
        .optional()
        .map_err(db_err)?
        .is_some())
}

impl SessionStore for SqliteSessionStore {
    fn create_thread<'a>(
        &'a self,
        parent: Option<ParentLink>,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ThreadMeta> {
        Self::ready(self.with_conn(move |conn| {
            let id = ThreadId::mint()?;
            let created_ms = now_ms();
            let attrs_json = serde_json::to_string(&attributes).map_err(json_err)?;
            let (parent_id, fork_at, inherits) = match &parent {
                Some(p) => (
                    Some(p.parent.as_str().to_string()),
                    Some(p.fork_at_seq as i64),
                    p.inherits,
                ),
                None => (None, None, true),
            };
            conn.execute(
                "INSERT INTO threads (id, parent, fork_at_seq, created_ms, attributes, inherits) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id.as_str(),
                    parent_id,
                    fork_at,
                    created_ms as i64,
                    attrs_json,
                    inherits
                ],
            )
            .map_err(db_err)?;
            Ok(ThreadMeta {
                id,
                parent,
                created_ms,
                last_seq: 0,
                attributes,
            })
        }))
    }

    fn meta<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Option<ThreadMeta>> {
        Self::ready(self.with_conn(|conn| {
            let row = conn
                .query_row(
                    "SELECT parent, fork_at_seq, created_ms, attributes, inherits \
                     FROM threads WHERE id = ?1",
                    params![id.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, Option<String>>(0)?,
                            r.get::<_, Option<i64>>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, bool>(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(db_err)?;
            let Some((parent_id, fork_at, created_ms, attrs_json, inherits)) = row else {
                return Ok(None);
            };
            // Next seq = MAX(seq)+1 (a single index seek on the (thread_id, seq)
            // PK), not COUNT(*) (an O(n) scan). Records are contiguous 0..n —
            // only whole threads are deleted — so this equals the record count.
            let last_seq: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM records WHERE thread_id = ?1",
                    params![id.as_str()],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            let parent = match (parent_id, fork_at) {
                (Some(p), Some(f)) => Some(ParentLink {
                    parent: ThreadId::new(p),
                    fork_at_seq: f as u64,
                    inherits,
                }),
                _ => None,
            };
            Ok(Some(ThreadMeta {
                id: id.clone(),
                parent,
                created_ms: created_ms as u64,
                last_seq: last_seq as u64,
                attributes: serde_json::from_str(&attrs_json).map_err(json_err)?,
            }))
        }))
    }

    fn threads<'a>(&'a self) -> SessionFuture<'a, Vec<ThreadMeta>> {
        Self::ready(self.with_conn(|conn| {
            // last_seq per thread via the same MAX(seq)+1 seek `meta` uses — a
            // correlated subquery on the (thread_id, seq) PK, not a COUNT scan.
            let mut stmt = conn
                .prepare(
                    "SELECT t.id, t.parent, t.fork_at_seq, t.created_ms, t.attributes, \
                            (SELECT COALESCE(MAX(seq), -1) + 1 FROM records r \
                             WHERE r.thread_id = t.id), t.inherits \
                     FROM threads t",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<i64>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, bool>(6)?,
                    ))
                })
                .map_err(db_err)?;
            let mut out = Vec::new();
            for row in rows {
                let (id, parent_id, fork_at, created_ms, attrs_json, last_seq, inherits) =
                    row.map_err(db_err)?;
                let parent = match (parent_id, fork_at) {
                    (Some(p), Some(f)) => Some(ParentLink {
                        parent: ThreadId::new(p),
                        fork_at_seq: f as u64,
                        inherits,
                    }),
                    _ => None,
                };
                out.push(ThreadMeta {
                    id: ThreadId::new(id),
                    parent,
                    created_ms: created_ms as u64,
                    last_seq: last_seq as u64,
                    attributes: serde_json::from_str(&attrs_json).map_err(json_err)?,
                });
            }
            Ok(out)
        }))
    }

    fn set_attributes<'a>(
        &'a self,
        id: &'a ThreadId,
        attributes: serde_json::Value,
    ) -> SessionFuture<'a, ()> {
        Self::ready(self.with_conn(move |conn| {
            let attrs_json = serde_json::to_string(&attributes).map_err(json_err)?;
            let updated = conn
                .execute(
                    "UPDATE threads SET attributes = ?2 WHERE id = ?1",
                    params![id.as_str(), attrs_json],
                )
                .map_err(db_err)?;
            if updated == 0 {
                return Err(SessionError::NotFound);
            }
            Ok(())
        }))
    }

    fn append<'a>(
        &'a self,
        id: &'a ThreadId,
        kind: RecordKind,
        data: serde_json::Value,
    ) -> SessionFuture<'a, Record> {
        Self::ready(self.with_conn(move |conn| {
            // IMMEDIATE takes the write lock at BEGIN, so a second writer (another
            // process sharing the DB) serializes here instead of racing the
            // seq SELECT and colliding on the (thread_id, seq) PK at INSERT.
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db_err)?;
            if !thread_exists(&tx, id)? {
                return Err(SessionError::NotFound);
            }
            // Next seq = MAX(seq)+1 (index seek), not COUNT(*) (O(n) scan).
            let seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq), -1) + 1 FROM records WHERE thread_id = ?1",
                    params![id.as_str()],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            let ts_ms = now_ms();
            let data_json = serde_json::to_string(&data).map_err(json_err)?;
            tx.execute(
                "INSERT INTO records (thread_id, seq, ts_ms, kind, data) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id.as_str(), seq, ts_ms as i64, kind_to_str(kind), data_json],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
            Ok(Record {
                seq: seq as u64,
                timestamp_ms: ts_ms,
                kind,
                data,
            })
        }))
    }

    fn records<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<Record>> {
        Self::ready(self.with_conn(|conn| {
            if !thread_exists(conn, id)? {
                return Err(SessionError::NotFound);
            }
            let mut stmt = conn
                .prepare(
                    "SELECT seq, ts_ms, kind, data FROM records WHERE thread_id = ?1 ORDER BY seq",
                )
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![id.as_str()], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })
                .map_err(db_err)?;
            let mut out = Vec::new();
            for row in rows {
                let (seq, ts_ms, kind, data) = row.map_err(db_err)?;
                out.push(Record {
                    seq: seq as u64,
                    timestamp_ms: ts_ms as u64,
                    kind: kind_from_str(&kind)?,
                    data: serde_json::from_str(&data).map_err(json_err)?,
                });
            }
            Ok(out)
        }))
    }

    fn children<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, Vec<ThreadId>> {
        Self::ready(self.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT id FROM threads WHERE parent = ?1")
                .map_err(db_err)?;
            let rows = stmt
                .query_map(params![id.as_str()], |r| r.get::<_, String>(0))
                .map_err(db_err)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(ThreadId::new(row.map_err(db_err)?));
            }
            Ok(out)
        }))
    }

    fn delete_thread<'a>(&'a self, id: &'a ThreadId) -> SessionFuture<'a, ()> {
        Self::ready(self.with_conn(|conn| {
            let tx = conn.transaction().map_err(db_err)?;
            // Refuse if a fork still points here — deleting would orphan it (the
            // `threads_parent` index makes this a cheap lookup).
            let has_children: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM threads WHERE parent = ?1)",
                    params![id.as_str()],
                    |r| r.get(0),
                )
                .map_err(db_err)?;
            if has_children {
                return Err(SessionError::HasChildren);
            }
            tx.execute(
                "DELETE FROM records WHERE thread_id = ?1",
                params![id.as_str()],
            )
            .map_err(db_err)?;
            tx.execute("DELETE FROM threads WHERE id = ?1", params![id.as_str()])
                .map_err(db_err)?;
            tx.commit().map_err(db_err)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::session::Session;

    fn store() -> Arc<dyn SessionStore> {
        Arc::new(SqliteSessionStore::open_in_memory().unwrap())
    }

    #[tokio::test]
    async fn round_trips_threads_records_and_children_via_the_index() {
        let store = store();
        let root = store
            .create_thread(None, serde_json::json!({ "owner": "alice" }))
            .await
            .unwrap();
        store
            .append(&root.id, RecordKind::Message, serde_json::json!({ "m": 0 }))
            .await
            .unwrap();
        store
            .append(
                &root.id,
                RecordKind::Checkpoint,
                serde_json::json!({ "s": "sum" }),
            )
            .await
            .unwrap();

        // meta reports the derived last_seq + the stored attributes.
        let meta = store.meta(&root.id).await.unwrap().unwrap();
        assert_eq!(meta.last_seq, 2);
        assert_eq!(meta.attributes, serde_json::json!({ "owner": "alice" }));

        // records come back in order, kinds preserved.
        let records = store.records(&root.id).await.unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].seq, 0);
        assert_eq!(records[0].kind, RecordKind::Message);
        assert_eq!(records[1].kind, RecordKind::Checkpoint);

        // children() is the indexed parent lookup.
        let child = store
            .create_thread(
                Some(ParentLink::fork(root.id.clone(), 1)),
                serde_json::json!({ "owner": "alice" }),
            )
            .await
            .unwrap();
        let kids = store.children(&root.id).await.unwrap();
        assert_eq!(kids, vec![child.id.clone()]);
        // the child's parent link round-trips.
        let child_meta = store.meta(&child.id).await.unwrap().unwrap();
        assert_eq!(child_meta.parent.as_ref().unwrap().parent, root.id);
        assert_eq!(child_meta.parent.as_ref().unwrap().fork_at_seq, 1);
        assert!(child_meta.parent.as_ref().unwrap().inherits);

        // delete refuses while a fork still points here (would orphan it)...
        assert!(matches!(
            store.delete_thread(&root.id).await.unwrap_err(),
            SessionError::HasChildren
        ));
        // ...delete the child first, then the parent is a hard delete.
        store.delete_thread(&child.id).await.unwrap();
        store.delete_thread(&root.id).await.unwrap();
        assert!(store.meta(&root.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn works_with_the_session_handle_fork_and_materialize() {
        let store = store();
        let parent = Session::create(store.clone(), None).await.unwrap();
        parent
            .append_message(serde_json::json!({ "m": 0 }))
            .await
            .unwrap();
        parent
            .append_message(serde_json::json!({ "m": 1 }))
            .await
            .unwrap();
        // Fork after the first message; the branch materializes [m0] + its own.
        let branch = parent.fork(1).await.unwrap();
        branch
            .append_message(serde_json::json!({ "b": 0 }))
            .await
            .unwrap();

        assert_eq!(parent.history().await.unwrap().len(), 2);
        let bh = branch.history().await.unwrap();
        assert_eq!(bh.len(), 2);
        assert_eq!(bh[0].data, serde_json::json!({ "m": 0 }));
        assert_eq!(bh[1].data, serde_json::json!({ "b": 0 }));

        // A spawn edge under that branch is lineage only: `inherits` survives
        // the column round-trip, and materialize stops rather than climbing to
        // the root's records.
        let spawned = store
            .create_thread(
                Some(ParentLink::spawn(branch.id().clone())),
                serde_json::Value::Null,
            )
            .await
            .unwrap();
        assert!(!spawned.parent.as_ref().unwrap().inherits);
        let spawned = Session::open(store.clone(), spawned.id);
        assert!(spawned.history().await.unwrap().is_empty());
        // …and it is still a child for the tree walk and the delete guard.
        let kids = store.children(branch.id()).await.unwrap();
        assert_eq!(kids.len(), 1);
        assert!(matches!(
            store.delete_thread(branch.id()).await.unwrap_err(),
            SessionError::HasChildren
        ));
    }

    #[test]
    fn a_lost_migration_race_is_recognized_rather_than_fatal() {
        // Pins the message match in `is_duplicate_column` against the real
        // SQLite error, since there is no distinct code to test for.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (id TEXT PRIMARY KEY, inherits INTEGER NOT NULL DEFAULT 1);",
        )
        .unwrap();
        let err = conn
            .execute(
                "ALTER TABLE threads ADD COLUMN inherits INTEGER NOT NULL DEFAULT 1",
                [],
            )
            .unwrap_err();
        assert!(is_duplicate_column(&err), "unrecognized: {err}");
        // A migration that lost the race still leaves the store openable.
        SqliteSessionStore::add_inherits_column(&conn).unwrap();
    }

    #[tokio::test]
    async fn a_database_written_before_spawn_edges_still_opens() {
        // The column is added by ALTER on an existing table, and every link such
        // a database holds is a fork — the column default.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE threads (
                 id          TEXT PRIMARY KEY,
                 parent      TEXT,
                 fork_at_seq INTEGER,
                 created_ms  INTEGER NOT NULL,
                 attributes  TEXT NOT NULL DEFAULT 'null'
             );
             INSERT INTO threads (id, parent, fork_at_seq, created_ms, attributes)
             VALUES ('t-old-child', 't-old-root', 3, 1, '{\"owner\":\"alice\"}');",
        )
        .unwrap();

        let store = SqliteSessionStore::from_connection(conn).unwrap();
        let meta = store
            .meta(&ThreadId::new("t-old-child"))
            .await
            .unwrap()
            .unwrap();
        let link = meta.parent.unwrap();
        assert_eq!(link.fork_at_seq, 3);
        assert!(
            link.inherits,
            "a pre-existing link must read back as a fork"
        );
    }

    #[tokio::test]
    async fn missing_thread_is_not_found_on_append() {
        let store = store();
        let err = store
            .append(
                &ThreadId::new("nope"),
                RecordKind::Message,
                serde_json::json!({}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::NotFound));
    }

    #[test]
    fn kind_column_strings_match_the_serde_representation() {
        // The hand-rolled `kind` map is kept (zero-alloc on the read path), but
        // locked to `RecordKind`'s serde wire form so the two can't drift.
        for kind in [
            RecordKind::Message,
            RecordKind::StateChange,
            RecordKind::Checkpoint,
        ] {
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::Value::String(kind_to_str(kind).to_string())
            );
            assert_eq!(kind_from_str(kind_to_str(kind)).unwrap(), kind);
        }
    }
}
