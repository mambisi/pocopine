/// Current Pocopine sync SQLite schema version.
pub const SCHEMA_VERSION: u32 = 3;

/// Metadata table for device identity and internal settings.
pub const META_TABLE: &str = "__pocopine_meta";
/// Per-stream cursor and collection metadata.
pub const STREAMS_TABLE: &str = "__pocopine_streams";
/// Persisted stream rows.
pub const ROWS_TABLE: &str = "__pocopine_rows";
/// Durable client mutation queue.
pub const MUTATIONS_TABLE: &str = "__pocopine_mutations";

/// Metadata key used to persist the stable sync device id.
pub const META_DEVICE_ID: &str = "device_id";
/// Metadata key used to persist the next mutation counter.
pub const META_NEXT_MUTATION_COUNTER: &str = "next_mutation_counter";
/// Metadata key used to persist the schema version.
pub const META_SCHEMA_VERSION: &str = "schema_version";

/// SQL statements used to bootstrap the local sync schema.
pub const BOOTSTRAP_SQL: &[&str] = &[
    "create table if not exists __pocopine_meta (
        key text primary key,
        value text not null
    )",
    "create table if not exists __pocopine_streams (
        stream text primary key,
        collection text not null,
        cursor text,
        schema_version integer not null,
        updated_at_ms integer not null
    )",
    "create table if not exists __pocopine_rows (
        stream text not null,
        row_key text not null,
        version text,
        payload text not null,
        pending integer not null default 0,
        conflict integer not null default 0,
        updated_at_ms integer not null,
        primary key (stream, row_key)
    )",
    "create table if not exists __pocopine_mutations (
        enqueue_seq integer primary key autoincrement,
        stream text not null,
        mutation_id text not null unique,
        row_key text,
        base_version text,
        op text not null,
        payload text,
        optimistic_row text,
        status text not null,
        error text,
        created_at_ms integer not null,
        updated_at_ms integer not null
    )",
    "create index if not exists __pocopine_rows_by_stream
        on __pocopine_rows (stream)",
    "create index if not exists __pocopine_mutations_by_stream_status
        on __pocopine_mutations (stream, status, enqueue_seq)",
];

/// SQL upsert used for stream cursor metadata.
pub const UPSERT_STREAM_SQL: &str = "insert into __pocopine_streams
    (stream, collection, cursor, schema_version, updated_at_ms)
    values (?1, ?2, ?3, ?4, ?5)
    on conflict(stream) do update set
        collection = excluded.collection,
        cursor = excluded.cursor,
        schema_version = excluded.schema_version,
        updated_at_ms = excluded.updated_at_ms";

/// SQL statements used to clear one stream before saving a replacement snapshot.
pub const DELETE_STREAM_ROWS_SQL: &str = "delete from __pocopine_rows where stream = ?1";

/// SQL upsert used for stream rows.
pub const UPSERT_ROW_SQL: &str = "insert into __pocopine_rows
    (stream, row_key, version, payload, pending, conflict, updated_at_ms)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
    on conflict(stream, row_key) do update set
        version = excluded.version,
        payload = excluded.payload,
        pending = excluded.pending,
        conflict = excluded.conflict,
        updated_at_ms = excluded.updated_at_ms";

/// SQL delete used for one row in one stream.
pub const DELETE_ROW_SQL: &str = "delete from __pocopine_rows where stream = ?1 and row_key = ?2";

/// SQL update used when a conflict references an existing local row.
pub const UPDATE_ROW_CONFLICT_SQL: &str =
    "update __pocopine_rows set pending = 0, conflict = 1, updated_at_ms = ?3
     where stream = ?1 and row_key = ?2";

/// SQL update used when a user resolves a local conflict marker.
pub const CLEAR_ROW_CONFLICT_SQL: &str =
    "update __pocopine_rows set conflict = 0, updated_at_ms = ?3
     where stream = ?1 and row_key = ?2";

/// SQL upsert used for durable pending mutations.
pub const UPSERT_MUTATION_SQL: &str = "insert into __pocopine_mutations
    (stream, mutation_id, row_key, base_version, op, payload, optimistic_row, status, error, created_at_ms, updated_at_ms)
    values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', null, ?8, ?8)
    on conflict(mutation_id) do update set
        stream = excluded.stream,
        row_key = excluded.row_key,
        base_version = excluded.base_version,
        op = excluded.op,
        payload = excluded.payload,
        optimistic_row = excluded.optimistic_row,
        status = 'pending',
        error = null,
        updated_at_ms = excluded.updated_at_ms";

/// SQL delete used when a mutation reaches a terminal server outcome.
pub const DELETE_MUTATION_SQL: &str = "delete from __pocopine_mutations where mutation_id = ?1";

/// SQL delete used by `SyncLocalStore::purge_pending_for_row`. Removes
/// every still-pending mutation for one `(stream, row_key)` pair.
/// Restricted to `status = 'pending'` — accepted / rejected / conflict
/// rows are historical outcomes, not queued edits, so they stay put.
pub const DELETE_PENDING_FOR_ROW_SQL: &str = "delete from __pocopine_mutations
    where stream = ?1 and row_key = ?2 and status = 'pending'";

/// SQL statements used by `SyncLocalStore::clear_all_streams`. Drops
/// every stream snapshot, cached row, and mutation queue entry across
/// the store. `__pocopine_meta` is intentionally NOT touched: the
/// device identity row and mutation counter must survive a sign-out so
/// future mutation ids stay globally unique against the server log.
pub const CLEAR_ALL_STREAMS_SQL: &[&str] = &[
    "delete from __pocopine_mutations",
    "delete from __pocopine_rows",
    "delete from __pocopine_streams",
];

/// SQL query used to load pending mutations for a stream in enqueue order.
pub const SELECT_PENDING_MUTATIONS_SQL: &str =
    "select mutation_id, row_key, base_version, op, payload, optimistic_row
    from __pocopine_mutations
    where stream = ?1 and status = 'pending'
    order by enqueue_seq asc";

/// SQL query used to hydrate rows for a stream.
pub const SELECT_ROWS_SQL: &str = "select row_key, version, payload, pending, conflict
    from __pocopine_rows
    where stream = ?1
    order by row_key asc";

/// SQL query used to hydrate stream metadata.
pub const SELECT_STREAM_SQL: &str =
    "select collection, cursor from __pocopine_streams where stream = ?1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_sql_mentions_all_internal_tables() {
        let joined = BOOTSTRAP_SQL.join("\n");

        assert!(joined.contains(META_TABLE));
        assert!(joined.contains(STREAMS_TABLE));
        assert!(joined.contains(ROWS_TABLE));
        assert!(joined.contains(MUTATIONS_TABLE));
    }
}
