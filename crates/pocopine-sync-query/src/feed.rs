//! Ordered change feed — the server half of the SPORC-style ordered
//! log: a per-stream, append-only change history with a truncation
//! watermark.
//!
//! The model (see `rfcs/` sync-query series and issue #292's
//! post-mortem for motivation): every state change a source accepts
//! is an explicit entry in a totally-ordered log — deletes included —
//! and a client syncs by pulling entries after its cursor. Absence
//! stops being a signal; the two protocol outcomes are "here are the
//! ops you missed, in order" and "your cursor predates my memory —
//! full re-sync" (the truncation watermark, made loud via
//! `SyncResyncReason::CursorTruncated`).
//!
//! # Who appends
//!
//! **The source does, inside the same transaction as the row write.**
//! The framework never appends post-hoc: a write that commits without
//! its feed entry punches a permanent hole in the log — an
//! incremental client simply never learns about the change. That is
//! the exact silent-absence class this module exists to kill, so the
//! append is part of the write, not a side effect after it. This is
//! why [`Source::create`]/[`update`]/[`delete`] receive a
//! [`WriteMeta`]: the mutation identity must be available *inside*
//! the transaction so the feed entry can carry its `origin` (the
//! feed echo that lets the originating client retire its pending
//! overlay even when the push response was lost).
//!
//! # Cursor semantics
//!
//! Cursors are opaque to clients; ordering is the log's private
//! business. A [`ChangeLog`] answers `list_since(cursor)` with either
//! a page of entries strictly after the cursor or `TooOld` — the
//! comparison against the watermark happens HERE, where the encoding
//! is known, never on the client.
//!
//! [`Source::create`]: crate::Source::create
//! [`update`]: crate::Source::update
//! [`delete`]: crate::Source::delete
//! [`WriteMeta`]: crate::WriteMeta

use std::sync::{Arc, Mutex};

use pocopine_core::server::RequestContext;
use pocopine_sync::{MutationId, RowKey, SyncCursor, SyncError, SyncResult};

use crate::query::Query;

/// One entry in a source's ordered change log.
#[derive(Clone, Debug)]
pub struct FeedEntry<Row> {
    /// Position in the stream's total order. Strictly increasing
    /// across entries; assigned by the log at append time.
    pub cursor: SyncCursor,
    /// The client mutation that produced this change, when the write
    /// came through the push path (the feed echo). `None` for
    /// server-side writes, imports, and migrations.
    pub origin: Option<MutationId>,
    /// What changed.
    pub change: FeedChangeKind<Row>,
}

/// The change an entry records. Deliberately smaller than
/// `SyncOp` — a feed has no `Reset`; "start over" is expressed
/// through the watermark/resync path instead.
#[derive(Clone, Debug)]
pub enum FeedChangeKind<Row> {
    /// The row identified by `key` was created or updated; `row` is
    /// its state as of this entry.
    Upsert { key: RowKey, row: Row },
    /// The row identified by `key` was deleted. An explicit op — the
    /// whole point of the feed.
    Delete { key: RowKey },
}

impl<Row> FeedChangeKind<Row> {
    /// The row key this entry is about.
    pub fn key(&self) -> &RowKey {
        match self {
            FeedChangeKind::Upsert { key, .. } => key,
            FeedChangeKind::Delete { key } => key,
        }
    }
}

/// A page of feed entries after a client's cursor.
#[derive(Clone, Debug)]
pub struct ChangePage<Row> {
    /// Entries strictly after the requested cursor, in log order.
    pub changes: Vec<FeedEntry<Row>>,
    /// The client's new cursor after applying `changes` — the last
    /// entry's position, or the requested cursor when the page is
    /// empty.
    pub cursor: SyncCursor,
    /// The log's current truncation watermark: the smallest cursor
    /// `list_since` can still serve incrementally.
    pub watermark: SyncCursor,
    /// More entries remain after this page (the caller's limit
    /// truncated it). The client pulls again from `cursor`.
    pub has_more: bool,
}

/// Outcome of [`ChangeLog::list_since`].
#[derive(Clone, Debug)]
pub enum ChangesSince<Row> {
    /// The cursor is serviceable — here is the ordered page.
    Page(ChangePage<Row>),
    /// The cursor predates the log's retained history. The caller
    /// must fall back to a full snapshot and mark it
    /// `SyncResyncReason::CursorTruncated` so the client re-syncs
    /// loudly instead of interpreting absences.
    TooOld {
        /// The smallest cursor the log can currently serve.
        watermark: SyncCursor,
    },
}

/// Ordered change log for one stream — the capability a [`Source`]
/// opts into via [`Source::change_log`].
///
/// Scoping contract: `list_since` MUST scope entries the same way
/// `list_stream` scopes rows (tenant filters from `query.params()`),
/// or serve a per-scope log. Leaking another tenant's changes
/// through the feed is exactly as bad as leaking their rows through
/// a snapshot.
///
/// [`Source`]: crate::Source
/// [`Source::change_log`]: crate::Source::change_log
#[async_trait::async_trait]
pub trait ChangeLog<Row>: Send + Sync + 'static
where
    Row: Clone + Send + Sync + 'static,
{
    /// Entries strictly after `cursor`, up to `limit`, in log order —
    /// or `TooOld` when the cursor predates retained history. The
    /// cursor-vs-watermark comparison happens here (the log knows its
    /// encoding; clients never order cursors).
    async fn list_since(
        &self,
        ctx: &RequestContext,
        query: &Query<Row>,
        cursor: &SyncCursor,
        limit: u32,
    ) -> SyncResult<ChangesSince<Row>>;

    /// The log's current head — the cursor a fresh snapshot should
    /// hand the client so its NEXT pull can be incremental. Read the
    /// head BEFORE listing snapshot rows: a write that lands between
    /// head-read and row-read is then covered twice (snapshot AND a
    /// later incremental entry — upserts are replay-safe), never
    /// zero times.
    async fn head(&self, ctx: &RequestContext, query: &Query<Row>) -> SyncResult<SyncCursor>;
}

// ─── In-memory reference implementation ─────────────────────────────

/// Row filter for [`MemoryChangeLog`] tenant-scoping in tests: given
/// the reconstructed query and an entry, decide whether the entry is
/// visible to that query's params.
pub type MemoryFeedFilter<Row> =
    Arc<dyn Fn(&Query<Row>, &FeedEntry<Row>) -> bool + Send + Sync + 'static>;

struct MemoryFeedInner<Row> {
    /// Next sequence number to assign (starts at 1).
    next_seq: u64,
    /// Entries with seq <= gc_floor have been truncated. The
    /// watermark cursor is exactly `gc_floor`: the smallest cursor
    /// `list_since` can serve from.
    gc_floor: u64,
    entries: Vec<(u64, FeedEntry<Row>)>,
}

/// Process-local change log for tests and single-process demos.
/// Sequence numbers encode as decimal-string cursors; a client
/// cursor of `"0"` (or a fresh snapshot against an empty log) means
/// "before everything".
///
/// Appends are synchronous under one mutex — the memory analogue of
/// "same transaction as the row write" when the paired source shares
/// the lock discipline (the reference sources in the test battery
/// append inside their own write path, before returning).
pub struct MemoryChangeLog<Row> {
    inner: Arc<Mutex<MemoryFeedInner<Row>>>,
    filter: Option<MemoryFeedFilter<Row>>,
}

impl<Row> Clone for MemoryChangeLog<Row> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            filter: self.filter.clone(),
        }
    }
}

impl<Row> std::fmt::Debug for MemoryChangeLog<Row> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryChangeLog").finish_non_exhaustive()
    }
}

impl<Row> Default for MemoryChangeLog<Row> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Row> MemoryChangeLog<Row> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryFeedInner {
                next_seq: 1,
                gc_floor: 0,
                entries: Vec::new(),
            })),
            filter: None,
        }
    }

    /// Attach a per-query visibility filter (tenant scoping in
    /// tests). Entries the filter rejects are skipped by
    /// `list_since` as if they never existed for that query.
    pub fn with_filter(mut self, filter: MemoryFeedFilter<Row>) -> Self {
        self.filter = Some(filter);
        self
    }

    fn lock(&self) -> SyncResult<std::sync::MutexGuard<'_, MemoryFeedInner<Row>>> {
        self.inner
            .lock()
            .map_err(|_| SyncError::backend("memory change log lock poisoned"))
    }

    /// Append an upsert entry. Returns the assigned cursor.
    pub fn record_upsert(
        &self,
        key: RowKey,
        row: Row,
        origin: Option<MutationId>,
    ) -> SyncResult<SyncCursor> {
        self.record(origin, FeedChangeKind::Upsert { key, row })
    }

    /// Append a delete entry. Returns the assigned cursor.
    pub fn record_delete(&self, key: RowKey, origin: Option<MutationId>) -> SyncResult<SyncCursor> {
        self.record(origin, FeedChangeKind::Delete { key })
    }

    fn record(
        &self,
        origin: Option<MutationId>,
        change: FeedChangeKind<Row>,
    ) -> SyncResult<SyncCursor> {
        let mut inner = self.lock()?;
        let seq = inner.next_seq;
        inner.next_seq = inner
            .next_seq
            .checked_add(1)
            .ok_or_else(|| SyncError::invalid_value("feed sequence", "overflow"))?;
        let cursor = SyncCursor::new(seq.to_string())?;
        let entry = FeedEntry {
            cursor: cursor.clone(),
            origin,
            change,
        };
        inner.entries.push((seq, entry));
        Ok(cursor)
    }

    /// Truncate history: forget every entry at or below `cursor`.
    /// A client whose cursor is now below the floor gets `TooOld`
    /// on its next pull — the loud path.
    pub fn gc_through(&self, cursor: &SyncCursor) -> SyncResult<()> {
        let seq = parse_seq(cursor)?;
        let mut inner = self.lock()?;
        inner.gc_floor = inner.gc_floor.max(seq);
        let floor = inner.gc_floor;
        inner.entries.retain(|(s, _)| *s > floor);
        Ok(())
    }

    /// Current head cursor without going through the trait (handy in
    /// tests that don't have a `RequestContext`).
    pub fn head_cursor(&self) -> SyncResult<SyncCursor> {
        let inner = self.lock()?;
        SyncCursor::new((inner.next_seq - 1).to_string())
    }
}

fn parse_seq(cursor: &SyncCursor) -> SyncResult<u64> {
    cursor
        .as_str()
        .parse::<u64>()
        .map_err(|_| SyncError::invalid_value("feed cursor", cursor.as_str()))
}

#[async_trait::async_trait]
impl<Row> ChangeLog<Row> for MemoryChangeLog<Row>
where
    Row: Clone + Send + Sync + 'static,
{
    async fn list_since(
        &self,
        _ctx: &RequestContext,
        query: &Query<Row>,
        cursor: &SyncCursor,
        limit: u32,
    ) -> SyncResult<ChangesSince<Row>> {
        // A cursor this log didn't mint (un-parseable) is
        // indistinguishable from "arbitrarily old" — answer TooOld
        // so the client re-syncs from a snapshot instead of the
        // server guessing.
        let inner = self.lock()?;
        let watermark = SyncCursor::new(inner.gc_floor.to_string())?;
        let Ok(since) = parse_seq(cursor) else {
            return Ok(ChangesSince::TooOld { watermark });
        };
        if since < inner.gc_floor {
            return Ok(ChangesSince::TooOld { watermark });
        }
        let mut changes: Vec<FeedEntry<Row>> = Vec::new();
        let mut has_more = false;
        for (seq, entry) in inner.entries.iter() {
            if *seq <= since {
                continue;
            }
            if let Some(filter) = &self.filter
                && !filter(query, entry)
            {
                continue;
            }
            if changes.len() as u32 >= limit {
                has_more = true;
                break;
            }
            changes.push(entry.clone());
        }
        let next_cursor = changes
            .last()
            .map(|e| e.cursor.clone())
            .unwrap_or_else(|| cursor.clone());
        Ok(ChangesSince::Page(ChangePage {
            changes,
            cursor: next_cursor,
            watermark,
            has_more,
        }))
    }

    async fn head(&self, _ctx: &RequestContext, _query: &Query<Row>) -> SyncResult<SyncCursor> {
        self.head_cursor()
    }
}
