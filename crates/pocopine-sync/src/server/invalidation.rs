use serde_json::Value;

use super::*;
use crate::{SyncError, SyncResult, sync_stream_tag};

impl SyncServer {
    /// Publish a live wake-up for one stream when an event backend is
    /// attached. The sync data still moves through pull; this only wakes
    /// browsers to pull with their current sync cursor.
    pub async fn invalidate_stream(&self, stream: &str) -> SyncResult<()> {
        let Some(events) = self.inner.events.as_ref() else {
            return Ok(());
        };
        let tag = sync_stream_tag(stream);
        let topic = pocopine_live::query_tag_topic(&tag)
            .map_err(|err| SyncError::backend(err.to_string()))?;
        let draft = pocopine_live::query_invalidated(topic, [tag])
            .map_err(|err| SyncError::backend(err.to_string()))?;
        events
            .publish(draft)
            .await
            .map_err(|err| SyncError::backend(err.to_string()))?;
        Ok(())
    }

    /// Publish a live wake-up scoped to the (stream, params_hash)
    /// audience the row's params identify (RFC 088 §C). Per-params
    /// ONLY — the caller is responsible for the bare-topic publish
    /// once per push (see the `/push` handler).
    ///
    /// If the stream's source overrides
    /// [`SyncStreamSource::row_to_params`] AND the projection
    /// returns non-empty params, publishes to
    /// `sync:stream:{stream}:{hash:016x}` so new clients listening
    /// on the per-params topic wake up. Sources that don't override
    /// (or that return empty params) make this a no-op — the
    /// caller's bare publish already reaches everyone.
    ///
    /// If `row_to_params` errors, the per-params publish is skipped
    /// and a `tracing::warn!` is emitted. The caller's bare publish
    /// (issued once per push, regardless of how many rows were
    /// accepted or whether any rows were returned) is the back-
    /// compat lifeline; a malformed row MUST NOT block it.
    pub async fn invalidate_stream_with_row(&self, stream: &str, row: &Value) -> SyncResult<()> {
        let Some(events) = self.inner.events.as_ref() else {
            return Ok(());
        };
        let registered = self.stream(stream)?;
        let params = match registered.source.row_to_params(row) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(
                    target: "pocopine.log",
                    stream = stream,
                    error = %err,
                    "RFC 088 §C: row_to_params failed; skipping per-params publish",
                );
                return Ok(());
            }
        };
        if params.is_empty() {
            // Source doesn't override row_to_params, or this row
            // projects to no required partition keys. The caller's
            // bare publish already covers everyone.
            return Ok(());
        }
        let hash = crate::stream_params_hash(stream, &params);
        let params_tag = crate::sync_stream_params_tag(stream, hash);
        let params_topic = pocopine_live::query_tag_topic(&params_tag)
            .map_err(|err| SyncError::backend(err.to_string()))?;
        let params_draft = pocopine_live::query_invalidated(params_topic, [params_tag])
            .map_err(|err| SyncError::backend(err.to_string()))?;
        events
            .publish(params_draft)
            .await
            .map_err(|err| SyncError::backend(err.to_string()))?;
        Ok(())
    }

    /// Batched variant of [`Self::invalidate_stream_with_row`] —
    /// project each row in `rows` to its params, DEDUPLICATE by
    /// `(stream, params_hash)`, and publish ONCE per distinct hash.
    ///
    /// The dedup matters for bulk pushes: a single push that
    /// accepts N rows all belonging to the same workspace (or
    /// other partition) would otherwise issue N identical publishes
    /// to the same topic — wasting broker capacity and waking
    /// matching clients N times only for them to /pull-with-cursor
    /// N times (the second through Nth are no-ops, but each costs
    /// a round-trip). With dedup the same push issues one publish
    /// per distinct hash.
    ///
    /// Per-row `row_to_params` errors are logged via
    /// `tracing::warn!` and skipped; the rest of the batch still
    /// publishes. The bare-topic publish is the caller's
    /// responsibility (issued once per push regardless of row
    /// count), so partial per-params publish failures don't break
    /// back-compat clients.
    pub async fn invalidate_stream_with_rows<'a, I>(&self, stream: &str, rows: I) -> SyncResult<()>
    where
        I: IntoIterator<Item = &'a Value>,
    {
        let Some(events) = self.inner.events.as_ref() else {
            return Ok(());
        };
        let registered = self.stream(stream)?;
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for row in rows {
            let params = match registered.source.row_to_params(row) {
                Ok(p) => p,
                Err(err) => {
                    tracing::warn!(
                        target: "pocopine.log",
                        stream = stream,
                        error = %err,
                        "RFC 088 §C: row_to_params failed; skipping per-params publish for row",
                    );
                    continue;
                }
            };
            if params.is_empty() {
                continue;
            }
            let hash = crate::stream_params_hash(stream, &params);
            if !seen.insert(hash) {
                // Same partition already published in this batch.
                continue;
            }
            let params_tag = crate::sync_stream_params_tag(stream, hash);
            let params_topic = pocopine_live::query_tag_topic(&params_tag)
                .map_err(|err| SyncError::backend(err.to_string()))?;
            let params_draft = pocopine_live::query_invalidated(params_topic, [params_tag])
                .map_err(|err| SyncError::backend(err.to_string()))?;
            events
                .publish(params_draft)
                .await
                .map_err(|err| SyncError::backend(err.to_string()))?;
        }
        Ok(())
    }
}
