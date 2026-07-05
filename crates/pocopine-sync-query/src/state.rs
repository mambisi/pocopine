//! `QueryState<Row>` — the per-query reactive state.
//!
//! Each `QuerySubscription` (TBD) owns one of these. Where
//! `pocopine_sync::CollectionState<T>` was a `Vec`-shaped state indexed
//! by position, `QueryState<Row>` is a `BTreeMap` indexed by `RowKey`
//! — because the routing engine inserts / removes rows by identity
//! (predicate evaluation), not by position.
//!
//! Rebase order:
//!
//! 1. Start with `canonical_rows`.
//! 2. Replay pending optimistic overlays in mutation-id order.
//! 3. Apply `order_by` + `limit` from the originating `Query`.
//! 4. Result: the visible row set.

use std::collections::BTreeMap;

use pocopine_sync::{
    ClientMutation, MutationId, RowKey, SyncCursor, SyncReason, SyncRow, SyncScope,
};
use serde::{Deserialize, Serialize};

/// Why a canonical row left the view on a settled pull.
///
/// The server snapshot stays authoritative either way — the row IS
/// removed from the view. The reason tells app-level recovery policy
/// what it may safely do about it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvictionReason {
    /// The authority carried a tombstone for this key: the row was
    /// positively deleted. Re-pushing it would resurrect data another
    /// device deliberately removed — don't.
    Deleted,
    /// The row was absent from a full snapshot with no tombstone. The
    /// engine can't tell "no longer matches the query's filter" from
    /// "lost at the backend" — that's app policy. A local-first app
    /// whose rows only ever leave the view by deletion may treat this
    /// as recoverable and re-push; an app with volatile filters should
    /// not.
    Unexplained,
}

/// One canonical row a settled pull removed from the view, plus why.
/// Drained via `QueryView::take_evictions`.
#[derive(Clone, Debug)]
pub struct EvictedRow<Row> {
    pub row: SyncRow<Row>,
    pub reason: EvictionReason,
}

/// Cap on the eviction report buffer. A consumer that never drains
/// must not turn the buffer into a leak; on overflow the OLDEST
/// entries drop first (the newest evictions are the actionable ones).
const MAX_EVICTIONS: usize = 256;

/// Per-query reactive state. Lives inside a `QuerySubscription`; readers
/// (via `QueryHandle`) get a reactive borrow.
///
/// Indexed by `RowKey` rather than position so the predicate-routing
/// engine can insert / remove rows by identity in O(log n).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryState<Row> {
    /// Server-confirmed canonical rows, keyed by row identity.
    canonical_rows: BTreeMap<RowKey, SyncRow<Row>>,
    /// In-flight optimistic mutations, in apply order.
    pending: Vec<PendingOverlay<Row>>,
    /// Rows settled pulls removed from the view, awaiting a
    /// `take_evictions` drain. In-memory only — hydrating a stale
    /// eviction report after reload would hand the app rows to
    /// re-judge that the next pull re-derives anyway.
    #[serde(skip)]
    evictions: Vec<EvictedRow<Row>>,
    /// Server cursor for the next incremental pull.
    pub cursor: Option<SyncCursor>,
    /// Schema version this state's canonical rows were fetched under.
    /// `None` means "never observed" (fresh state); compared on every
    /// open to detect schema drift on this query's compartment.
    pub application_schema_version: Option<u32>,
    /// Last principal PROVIDED BY THE IMPLEMENTOR that this state's
    /// canonical rows were settled under (via `Source::scope` /
    /// `SyncStreamSource::scope` on the server). The engine does not
    /// model users, sessions, or anonymity — it only compares two
    /// provided tokens for equality: when both this and a response's
    /// scope are `Some` and differ, the local state belongs to a
    /// different principal and the engine clears everything and
    /// re-syncs. `None` (here or on a response) simply means "no
    /// principal provided" and never triggers anything. Implementors
    /// that want session-expiry or anonymous transitions detected
    /// return a token for those sessions too.
    pub sync_scope: Option<SyncScope>,
    /// True while the first hydrate + pull are in flight.
    pub loading: bool,
    /// True while a background pull / push is in flight after the first.
    pub syncing: bool,
    /// True after an accepted push; cleared by the next pull.
    pub stale: bool,
    /// Last error message; empty when no error.
    pub error: String,
    /// Last reason for a state transition.
    pub last_reason: SyncReason,
    /// In-memory generation token; bumped on wipe/reset so in-flight
    /// tasks can detect staleness via `is_current(token)`.
    #[serde(skip)]
    request_generation: u64,
    /// Monotonically-bumped version counter that ticks on every
    /// state-mutating operation (canonical upsert / remove, pending
    /// push / remove, reset). Observers register pocopine effects
    /// that track this field; any internal state change re-fires
    /// the effect via `pocopine_core::track(...)` on the same key.
    #[serde(skip)]
    version: u64,
}

/// One in-flight optimistic mutation overlaid on top of canonical state.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingOverlay<Row> {
    pub mutation_id: MutationId,
    pub mutation: ClientMutation<serde_json::Value>,
    /// Rendered optimistic row, if the mutation produces one (Upsert);
    /// `None` for Delete-shaped mutations.
    pub optimistic_row: Option<SyncRow<Row>>,
    /// Snapshot of the canonical row a Delete overlay removed
    /// optimistically — populated only on Delete branches that
    /// actually evicted a canonical row. The rollback path
    /// re-upserts this row into canonical state if the server
    /// rejects the delete; otherwise the canonical reconcile leaves
    /// it as-is (the server's confirmation supersedes the snapshot).
    pub deleted_row: Option<SyncRow<Row>>,
    /// Key this overlay should suppress in the rendered view.
    /// Populated on `RowChange::Delete` overlays AND on optimistic
    /// predicate-departure overlays (an `Upsert` that no longer
    /// matches the subscription's predicate). The merge in
    /// [`crate::QueryView::rows`] removes this key from the rendered
    /// row set so a Delete made against a row still pending its
    /// canonical reconcile (visible only via a prior optimistic
    /// `Upsert` overlay) hides correctly. `None` for Upsert
    /// overlays — those insert rather than evict.
    pub evicted_key: Option<RowKey>,
    /// Set when a server response surfaces a conflict for this
    /// mutation; the overlay stays visible with the conflict flag
    /// until the user resolves via the UI.
    pub conflict: bool,
}

impl<Row> Default for QueryState<Row> {
    // Manual impl avoids the `Row: Default` bound that `derive(Default)`
    // would add — the `Row` only appears inside `Vec` / `BTreeMap`
    // values, all of which have their own empty constructors.
    fn default() -> Self {
        Self {
            canonical_rows: BTreeMap::new(),
            pending: Vec::new(),
            evictions: Vec::new(),
            cursor: None,
            application_schema_version: None,
            sync_scope: None,
            loading: false,
            syncing: false,
            stale: false,
            error: String::new(),
            last_reason: pocopine_sync::SyncReason::Idle,
            request_generation: 0,
            version: 0,
        }
    }
}

impl<Row> QueryState<Row> {
    /// Monotonically-bumped version. Observers read this to detect
    /// any state change without diffing the full state.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Borrow the canonical rows in key order.
    pub fn canonical_rows(&self) -> impl Iterator<Item = &SyncRow<Row>> {
        self.canonical_rows.values()
    }

    /// O(log n) lookup for a canonical row by key. Used by the routing
    /// engine and view layer to avoid the O(n) `canonical_rows().find()`
    /// pattern that scales badly for subscriptions holding thousands of
    /// rows.
    pub fn canonical_get(&self, key: &RowKey) -> Option<&SyncRow<Row>> {
        self.canonical_rows.get(key)
    }

    /// True iff `key` is currently in canonical state.
    pub fn canonical_contains(&self, key: &RowKey) -> bool {
        self.canonical_rows.contains_key(key)
    }

    /// Number of canonical rows currently held.
    pub fn canonical_len(&self) -> usize {
        self.canonical_rows.len()
    }

    /// Pending overlays in apply order.
    pub fn pending(&self) -> &[PendingOverlay<Row>] {
        &self.pending
    }

    /// Generation token for in-flight work: every `reset()` bumps it,
    /// so a task that captured the token before an `.await` can
    /// detect that the state its request was issued for no longer
    /// exists (scope-drift fence, schema wipe) and discard the
    /// response instead of settling it.
    pub fn request_generation(&self) -> u64 {
        self.request_generation
    }

    /// Rows settled pulls have removed from the view since the last
    /// drain, oldest first. Prefer `QueryView::take_evictions` — a
    /// drain — so each eviction is judged once; this accessor exists
    /// for read-only inspection.
    pub fn evictions(&self) -> &[EvictedRow<Row>] {
        &self.evictions
    }
}

// `remove_canonical` / `push_pending` / `remove_pending` are entrypoints
// the (TBD) routing engine will call. Silenced until the engine lands.
#[allow(dead_code)]
impl<Row: Clone> QueryState<Row> {
    /// Drop every canonical row + pending overlay; bump the generation
    /// token so any in-flight task that touches this state via
    /// `apply_*` becomes a no-op. Used on schema-drift wipes.
    pub fn reset(&mut self) {
        self.canonical_rows.clear();
        self.pending.clear();
        self.evictions.clear();
        self.cursor = None;
        self.loading = false;
        self.syncing = false;
        self.stale = false;
        self.error.clear();
        self.last_reason = SyncReason::Initial;
        self.request_generation = self.request_generation.saturating_add(1);
        self.bump_version();
    }

    /// Tick the version counter. Called by every state-mutating
    /// method below so observers can react to any change without
    /// diffing.
    fn bump_version(&mut self) {
        self.version = self.version.saturating_add(1);
    }

    /// Internal helper for the routing engine.
    pub(crate) fn upsert_canonical(&mut self, row: SyncRow<Row>) {
        self.canonical_rows.insert(row.key.clone(), row);
        self.bump_version();
    }

    /// Internal helper for the routing engine.
    pub(crate) fn remove_canonical(&mut self, key: &RowKey) {
        if self.canonical_rows.remove(key).is_some() {
            self.bump_version();
        }
    }

    /// Internal helper for the routing engine.
    pub(crate) fn push_pending(&mut self, overlay: PendingOverlay<Row>) {
        self.pending.push(overlay);
        self.bump_version();
    }

    /// Append eviction records from a settled pull, keeping the buffer
    /// bounded (oldest entries drop first past [`MAX_EVICTIONS`]).
    pub(crate) fn record_evictions(&mut self, evicted: Vec<EvictedRow<Row>>) {
        if evicted.is_empty() {
            return;
        }
        self.evictions.extend(evicted);
        if self.evictions.len() > MAX_EVICTIONS {
            let excess = self.evictions.len() - MAX_EVICTIONS;
            self.evictions.drain(..excess);
        }
        self.bump_version();
    }

    /// Drain the eviction report. Each entry is handed out exactly
    /// once.
    pub(crate) fn take_evictions(&mut self) -> Vec<EvictedRow<Row>> {
        if self.evictions.is_empty() {
            return Vec::new();
        }
        // Draining is a state change observers may care about (an
        // "n unsynced rows" badge clearing) — bump like every other
        // mutation.
        self.bump_version();
        std::mem::take(&mut self.evictions)
    }

    /// Internal helper for the routing engine. Removes EVERY pending
    /// overlay attached to `id` (called when the server accepts or
    /// rejects the mutation). A single mutation can produce multiple
    /// `RowChange`s — each gets its own overlay with the same
    /// `mutation_id`, so the dequeue path must clear them all.
    /// Returns the removed overlays (caller may inspect them to
    /// restore optimistic-delete state on rollback).
    pub(crate) fn remove_pending(&mut self, id: &MutationId) -> Vec<PendingOverlay<Row>> {
        let mut removed = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            if &self.pending[i].mutation_id == id {
                removed.push(self.pending.remove(i));
            } else {
                i += 1;
            }
        }
        if !removed.is_empty() {
            self.bump_version();
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_is_empty() {
        let s: QueryState<String> = QueryState::default();
        assert_eq!(s.canonical_len(), 0);
        assert!(s.pending().is_empty());
        assert!(s.cursor.is_none());
        assert!(s.application_schema_version.is_none());
        assert!(!s.loading);
    }

    #[test]
    fn reset_drops_state_and_bumps_generation() {
        let mut s: QueryState<String> = QueryState::default();
        s.upsert_canonical(SyncRow::new("row_1", "hello".to_string()).unwrap());
        s.cursor = Some(SyncCursor::new("cursor_1").unwrap());
        let gen_before = s.request_generation;

        s.reset();
        assert_eq!(s.canonical_len(), 0);
        assert!(s.cursor.is_none());
        assert!(s.request_generation > gen_before);
    }
}
