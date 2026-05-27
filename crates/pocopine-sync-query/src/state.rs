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

use pocopine_sync::{ClientMutation, MutationId, RowKey, SyncCursor, SyncReason, SyncRow};
use serde::{Deserialize, Serialize};

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
    /// Server cursor for the next incremental pull.
    pub cursor: Option<SyncCursor>,
    /// Schema version this state's canonical rows were fetched under.
    /// `None` means "never observed" (fresh state); compared on every
    /// open to detect schema drift on this query's compartment.
    pub application_schema_version: Option<u32>,
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
            cursor: None,
            application_schema_version: None,
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

    /// Number of canonical rows currently held.
    pub fn canonical_len(&self) -> usize {
        self.canonical_rows.len()
    }

    /// Pending overlays in apply order.
    pub fn pending(&self) -> &[PendingOverlay<Row>] {
        &self.pending
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

    /// Internal helper for the routing engine. Removes a pending overlay
    /// by mutation id (called when the server accepts/rejects the
    /// mutation).
    pub(crate) fn remove_pending(&mut self, id: &MutationId) -> Option<PendingOverlay<Row>> {
        let idx = self.pending.iter().position(|p| &p.mutation_id == id)?;
        let removed = self.pending.remove(idx);
        self.bump_version();
        Some(removed)
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
