//! The Yjs sync primitives over a `yrs` document (RFC 073, the collab wire
//! protocol). These are the server-side half of the handshake:
//!
//! - `state_vector()` → SyncStep1 payload ("what I have")
//! - `diff(remote_sv)` → SyncStep2 payload ("what you're missing")
//! - `apply_update()` → merge a remote Update (idempotent, commutative)
//! - `full_update()` → the whole doc as one update (a snapshot blob, or the
//!   catch-up for a brand-new peer)
//!
//! CRDT merges are order-independent: applying updates in any order, even
//! twice, converges. So the per-topic `seq` from the gateway is only for
//! replay/resume bookkeeping, never for merge correctness.

use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Doc, GetString, ReadTxn, StateVector, Text, Transact, Update};

use super::error::{CollabError, CollabResult};

/// A collaborative document: a thin wrapper over `yrs::Doc` exposing the sync
/// handshake primitives.
pub struct CollabDocument {
    doc: Doc,
}

impl CollabDocument {
    /// A fresh, empty document.
    pub fn new() -> Self {
        Self { doc: Doc::new() }
    }

    /// Reconstruct a document from a persisted full-state update (a snapshot
    /// blob from the `CollabStore`).
    pub fn from_snapshot(blob: &[u8]) -> CollabResult<Self> {
        let doc = Self::new();
        doc.apply_update(blob)?;
        Ok(doc)
    }

    /// Borrow the underlying `yrs` document (to attach observers, drive edits,
    /// or read shared types directly).
    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Apply a remote update — a CRDT delta, a SyncStep2 catch-up, or a full
    /// snapshot. Idempotent: re-applying a known update is a no-op.
    pub fn apply_update(&self, bytes: &[u8]) -> CollabResult<()> {
        let update =
            Update::decode_v1(bytes).map_err(|err| CollabError::Decode(err.to_string()))?;
        self.doc
            .transact_mut()
            .apply_update(update)
            .map_err(|err| CollabError::Apply(err.to_string()))?;
        Ok(())
    }

    /// This document's state vector (the SyncStep1 payload), encoded.
    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// The update a peer whose state vector is `remote_sv` is missing (the
    /// SyncStep2 payload).
    pub fn diff(&self, remote_sv: &[u8]) -> CollabResult<Vec<u8>> {
        let sv = StateVector::decode_v1(remote_sv)
            .map_err(|err| CollabError::Decode(err.to_string()))?;
        Ok(self.doc.transact().encode_state_as_update_v1(&sv))
    }

    /// The whole document as a single update (a snapshot blob, equivalently the
    /// diff a brand-new peer with an empty state vector is missing).
    pub fn full_update(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }

    // --- convenience for tests / simple text docs ---

    /// Insert `content` into a root text field at `index`.
    pub fn insert_text(&self, field: &str, index: u32, content: &str) {
        let text = self.doc.get_or_insert_text(field);
        let mut txn = self.doc.transact_mut();
        text.insert(&mut txn, index, content);
    }

    /// Remove `len` characters from a root text field at `index`. A delete is
    /// recorded in the delete-set, not the state vector.
    pub fn delete_text(&self, field: &str, index: u32, len: u32) {
        let text = self.doc.get_or_insert_text(field);
        let mut txn = self.doc.transact_mut();
        text.remove_range(&mut txn, index, len);
    }

    /// Read a root text field as a string.
    pub fn text(&self, field: &str) -> String {
        let text = self.doc.get_or_insert_text(field);
        text.get_string(&self.doc.transact())
    }
}

impl Default for CollabDocument {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_documents_converge_via_diff_exchange() {
        let a = CollabDocument::new();
        let b = CollabDocument::new();
        a.insert_text("body", 0, "hello");
        b.insert_text("body", 0, "world");

        // Each side computes what the other is missing from its state vector.
        let a_missing = b.diff(&a.state_vector()).unwrap();
        let b_missing = a.diff(&b.state_vector()).unwrap();
        a.apply_update(&a_missing).unwrap();
        b.apply_update(&b_missing).unwrap();

        // CRDT convergence: identical result on both sides...
        assert_eq!(a.text("body"), b.text("body"));
        // ...containing both concurrent edits.
        let merged = a.text("body");
        assert!(merged.contains("hello") && merged.contains("world"));
    }

    #[test]
    fn fresh_peer_catches_up_from_full_diff() {
        let server = CollabDocument::new();
        server.insert_text("body", 0, "shared state");

        // A brand-new peer sends its (empty) state vector; the server replies
        // with the diff (SyncStep1 -> SyncStep2).
        let fresh = CollabDocument::new();
        let catch_up = server.diff(&fresh.state_vector()).unwrap();
        fresh.apply_update(&catch_up).unwrap();

        assert_eq!(fresh.text("body"), "shared state");
    }

    #[test]
    fn apply_is_idempotent_and_order_independent() {
        let source = CollabDocument::new();
        source.insert_text("body", 0, "abc");
        let u1 = source.full_update();
        source.insert_text("body", 3, "def");
        let u2 = source
            .diff(&{
                let d = CollabDocument::new();
                d.apply_update(&u1).unwrap();
                d.state_vector()
            })
            .unwrap();

        // Apply out of order and twice; still converges to the source.
        let target = CollabDocument::new();
        target.apply_update(&u2).unwrap(); // the later delta first
        target.apply_update(&u1).unwrap();
        target.apply_update(&u1).unwrap(); // duplicate
        assert_eq!(target.text("body"), source.text("body"));
        assert_eq!(target.text("body"), "abcdef");
    }

    #[test]
    fn state_vector_round_trips_through_a_snapshot() {
        let doc = CollabDocument::new();
        doc.insert_text("body", 0, "persist me");
        let blob = doc.full_update();

        let restored = CollabDocument::from_snapshot(&blob).unwrap();
        assert_eq!(restored.text("body"), "persist me");
    }
}
