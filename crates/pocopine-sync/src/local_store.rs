mod types;

#[cfg(test)]
mod tests;

pub use types::*;

use crate::{ClientMutation, MutationId, RowKey, SyncError, SyncStreamName};

/// Durable client-side storage contract for sync streams.
///
/// Implementations must apply snapshot, change, mutation enqueue, and push
/// result operations atomically. The local store improves durability and
/// startup latency; it is not an authorization boundary.
///
/// `reserve_mutation_id` is the safe allocation boundary for stable mutation
/// ids. Implementations must persist the incremented counter before returning
/// the id, so a reload or failed network request cannot reuse it.
pub trait SyncLocalStore {
    /// Load the persisted client identity, if this store has one.
    fn load_identity(&self) -> SyncLocalFuture<'_, Option<SyncLocalIdentity>>;

    /// Persist the client identity and next mutation counter.
    fn save_identity(&self, identity: SyncLocalIdentity) -> SyncLocalFuture<'_, ()>;

    /// Reserve a durable mutation id for the local device.
    fn reserve_mutation_id(&self) -> SyncLocalFuture<'_, MutationId>;

    /// Hydrate locally cached rows and pending mutations for a stream.
    fn hydrate_stream(&self, stream: &SyncStreamName) -> SyncLocalFuture<'_, LocalStreamSnapshot>;

    /// Atomically replace the locally cached stream snapshot.
    fn save_snapshot(&self, snapshot: LocalSnapshotBatch) -> SyncLocalFuture<'_, ()>;

    /// Atomically apply incremental server changes for a stream.
    fn apply_changes(&self, changes: LocalChangeBatch) -> SyncLocalFuture<'_, ()>;

    /// Persist a mutation before it is sent to the server.
    fn enqueue_mutation(
        &self,
        stream: &SyncStreamName,
        mutation: ClientMutation<serde_json::Value>,
    ) -> SyncLocalFuture<'_, ()>;

    /// Persist a local pending mutation and its optional optimistic row before
    /// the mutation is sent to the server.
    fn enqueue_pending_mutation(
        &self,
        stream: &SyncStreamName,
        pending: LocalPendingMutation,
    ) -> SyncLocalFuture<'_, ()> {
        self.enqueue_mutation(stream, pending.mutation)
    }

    /// Persist accepted, rejected, or conflicted mutation outcomes.
    ///
    /// Row `pending` flags persisted here describe the latest server outcome.
    /// With stacked pending mutations for the same key, hydrated rows may
    /// understate pending UI state until the client replays the queued
    /// mutations.
    fn mark_push_result(&self, result: LocalPushResult) -> SyncLocalFuture<'_, ()>;

    /// Clear a persisted row conflict marker after the user resolves it.
    ///
    /// This only clears local metadata. It does not write application data to
    /// the server and does not remove any still-pending mutations for the row.
    fn clear_conflict(&self, stream: &SyncStreamName, key: &RowKey) -> SyncLocalFuture<'_, ()>;

    /// Load pending mutations that should be replayed for one stream.
    fn pending_mutations(
        &self,
        stream: &SyncStreamName,
    ) -> SyncLocalFuture<'_, Vec<ClientMutation<serde_json::Value>>>;

    /// Durably remove every still-pending mutation whose `key` matches
    /// the given row, scoped to one stream. Returns the number of
    /// mutations removed.
    ///
    /// This is the storage primitive behind
    /// `CrudClientResource::discard_local`: it lets the author-facing
    /// helper back out a row's queued edits with the same durability
    /// guarantees as `enqueue_mutation` — a reload cannot resurrect
    /// the purged mutations.
    ///
    /// Implementations MUST:
    ///
    /// * Persist the removal before returning. The whole point of a
    ///   durable purge is that reload doesn't replay the dropped
    ///   mutations.
    /// * Leave mutations whose `key` is `None` or differs from `key`
    ///   untouched — purging one row must not touch another row's
    ///   queue.
    /// * Leave non-pending records (accepted / rejected / conflict
    ///   history) alone. Those are server outcomes, not local edits.
    /// * Leave the row's conflict marker untouched; clearing it is
    ///   `clear_conflict`'s job and the caller composes the two.
    ///
    /// The default implementation falls back to no-op + `0` for stores
    /// that haven't been updated yet — existing impls without this
    /// method continue to compile, but generated `discard_local`
    /// helpers will silently leave durable entries behind until the
    /// store opts in.
    fn purge_pending_for_row(
        &self,
        _stream: &SyncStreamName,
        _key: &RowKey,
    ) -> SyncLocalFuture<'_, usize> {
        Box::pin(std::future::ready(Ok(0)))
    }

    /// Durably drop every persisted stream snapshot, cached row, pending
    /// mutation, and conflict marker. The persisted `SyncLocalIdentity`
    /// (device id + mutation counter) is left intact so future mutation
    /// ids stay globally unique against any server log entry the local
    /// store has already produced.
    ///
    /// This is the storage primitive behind `SyncClient::sign_out` and
    /// per-tenant cache resets. Authorization is enforced by the server
    /// on every `/open`, `/pull`, and `/push`; the local store is not a
    /// trust boundary and may legitimately hold rows that were once
    /// visible to the previous user. Apps that switch users, tenants,
    /// or auth scopes call this helper to drop the stale cache before
    /// re-mounting collections.
    ///
    /// Implementations MUST:
    ///
    /// * Persist the wipe before returning. Reload must observe an empty
    ///   sync cache, not the pre-wipe state.
    /// * Leave the device identity row untouched. Rotating the device id
    ///   would silently reset the mutation counter and could collide with
    ///   any accepted-log entry the server still retains.
    /// * Be safe to call on a never-populated store — calling
    ///   `clear_all_streams` on a fresh install is a successful no-op.
    ///
    /// The default implementation returns `SyncError::Unsupported` so an
    /// out-of-tree store that hasn't opted in cannot silently leave
    /// durable data behind on sign-out. Auth/tenant boundary helpers
    /// MUST surface "this store does not support a durable wipe" rather
    /// than acting like the wipe succeeded.
    fn clear_all_streams(&self) -> SyncLocalFuture<'_, ()> {
        Box::pin(std::future::ready(Err(SyncError::unsupported(
            "SyncLocalStore::clear_all_streams is not implemented for this store",
        ))))
    }

    /// Durably drop every persisted row, pending mutation, conflict
    /// marker, and stream-metadata row for ONE stream — leaving every
    /// other stream and the device identity row intact.
    ///
    /// This is the storage primitive behind the client-side schema-bump
    /// cache invalidation: when `/open` advertises a `schema_version`
    /// different from the cached `application_schema_version` for the
    /// stream, the client awaits `clear_stream` before hydrating so
    /// stale rows + queued mutations encoded against the old shape
    /// don't reach the in-memory `CollectionState`. Authors that opt
    /// into the per-stream wipe also use it for tenant-switcher UIs
    /// that re-scope a stream without a full sign-out.
    ///
    /// Implementations MUST:
    ///
    /// * Persist the wipe before returning. Reload must observe an empty
    ///   cache for this stream, not the pre-wipe state.
    /// * Leave other streams' rows + pending mutations untouched.
    /// * Leave the device identity row untouched.
    /// * Be safe to call on a stream that's never been persisted — a
    ///   per-stream wipe on a fresh install is a successful no-op.
    /// * Wipe atomically: a mid-wipe failure must either revert
    ///   entirely or complete entirely (SQLite-WAL backed stores wrap
    ///   the deletes in `BEGIN IMMEDIATE TRANSACTION`).
    ///
    /// The default implementation returns `SyncError::Unsupported` so
    /// an out-of-tree store that hasn't opted in cannot silently leave
    /// durable data behind on a schema bump.
    fn clear_stream(&self, _stream: &SyncStreamName) -> SyncLocalFuture<'_, ()> {
        Box::pin(std::future::ready(Err(SyncError::unsupported(
            "SyncLocalStore::clear_stream is not implemented for this store",
        ))))
    }
}
