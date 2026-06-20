//! The live binding: [`CollabEditor`] syncs a pine-richtext document `Node`
//! with a yrs CRDT `Doc` so edits converge across replicas over the
//! `pocopine-collab` transport.
//!
//! ## v1 scope — single-writer / convergence-only
//!
//! [`CollabEditor::set_document`] re-encodes the whole document (a **coarse**
//! write). It is correct for one author at a time but, per edit, tombstones the
//! old tree and emits an update proportional to the whole doc. The incremental,
//! `Step`-driven write — efficient and multi-writer, preserving a second
//! writer's cursor via `StickyIndex` — is the v2 follow-up. The read path
//! ([`CollabEditor::apply_remote`] → whole-doc decode) is coarse by design,
//! which is exactly why v1 cannot keep two live cursors from fighting.
//!
//! ## App glue
//!
//! The binder speaks `Node`, so wiring it to a live editor is trivial:
//! `apply_remote` returns the `Node` to load into an `EditorState`, and a local
//! `EditorState` change is committed via `set_document(state.doc())`.
//!
//! ## Origins (the three-origin contract)
//!
//! Local writes are tagged `"pm"`, inbound updates `"remote"`. The explicit
//! binder drives both directions itself, so it does not yet rely on these — but
//! they are the contract a future `observe_deep`-driven binding needs to avoid
//! feedback loops, so they are established here.

use std::fmt;

use pine_richtext::model::{Node, Schema};
use pine_richtext::{RichTextError, RichTextResult};
use yrs::types::xml::{XmlFragment, XmlFragmentRef};
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

use crate::{decode_doc, encode_doc};

const ORIGIN_LOCAL: &str = "pm";
const ORIGIN_REMOTE: &str = "remote";

/// Error from the live binding.
#[derive(Debug)]
pub enum BindError {
    /// A yrs update could not be decoded or applied.
    Crdt(String),
    /// The document model rejected a node (schema validation / codec).
    Model(RichTextError),
}

impl From<RichTextError> for BindError {
    fn from(err: RichTextError) -> Self {
        Self::Model(err)
    }
}

impl fmt::Display for BindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Crdt(msg) => write!(f, "collab crdt error: {msg}"),
            Self::Model(err) => write!(f, "collab model error: {err}"),
        }
    }
}

impl std::error::Error for BindError {}

/// A pine-richtext document bound to a yrs CRDT.
pub struct CollabEditor {
    doc: Doc,
    root: XmlFragmentRef,
    schema: Schema,
    client_id: u64,
    block_seq: u64,
}

impl CollabEditor {
    /// A new, empty editor with a framework-assigned, globally-unique
    /// `client_id` (e.g. the realtime `ws-N` session number) — RNG-free, so it
    /// works on wasm without `getrandom`.
    pub fn new(client_id: u64, schema: Schema) -> Self {
        let doc = Doc::with_client_id(client_id);
        let root = doc.get_or_insert_xml_fragment("doc");
        Self {
            doc,
            root,
            schema,
            client_id,
            block_seq: 0,
        }
    }

    /// The current document as a pine-richtext `Node`.
    pub fn document(&self) -> RichTextResult<Node> {
        decode_doc(&self.doc.transact(), &self.root, &self.schema)
    }

    /// Replace the document with a locally-edited one (the coarse v1 write).
    /// Returns the yrs update to broadcast — the diff since the prior state.
    pub fn set_document(&mut self, doc: &Node) -> RichTextResult<Vec<u8>> {
        let before = self.doc.transact().state_vector();
        let client_id = self.client_id;
        let mut seq = self.block_seq;
        {
            let mut txn = self.doc.transact_mut_with(ORIGIN_LOCAL.to_string());
            let len = self.root.len(&txn);
            self.root.remove_range(&mut txn, 0, len);
            let mut next_block_id = || {
                seq += 1;
                format!("{client_id}-{seq}")
            };
            encode_doc(&mut txn, &self.root, doc, &self.schema, &mut next_block_id)?;
        }
        self.block_seq = seq;
        Ok(self.doc.transact().encode_diff_v1(&before))
    }

    /// Apply an inbound network update; returns the new document.
    pub fn apply_remote(&mut self, update: &[u8]) -> Result<Node, BindError> {
        let update = Update::decode_v1(update).map_err(|err| BindError::Crdt(err.to_string()))?;
        {
            let mut txn = self.doc.transact_mut_with(ORIGIN_REMOTE.to_string());
            txn.apply_update(update)
                .map_err(|err| BindError::Crdt(err.to_string()))?;
        }
        self.document().map_err(BindError::from)
    }

    /// A full-state update that bootstraps another editor onto this document
    /// (the catch-up a freshly-joined peer applies).
    pub fn full_update(&self) -> Vec<u8> {
        self.doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::schema_basic;

    fn para(text: &str) -> Node {
        schema_basic::paragraph(vec![schema_basic::text(text, vec![]).unwrap()]).unwrap()
    }

    #[test]
    fn two_editors_converge_through_updates() {
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());

        // A authors a document and broadcasts the update.
        let doc_a = schema_basic::doc(vec![para("hello")]).unwrap();
        let update = a.set_document(&doc_a).unwrap();

        // B applies it and converges to A's document.
        let got = b.apply_remote(&update).unwrap();
        assert_eq!(got, doc_a, "B should decode A's document");
        assert_eq!(a.document().unwrap(), b.document().unwrap());

        // B edits (appends a paragraph); A applies and converges to B's doc.
        let doc_b = schema_basic::doc(vec![para("hello"), para("world")]).unwrap();
        let update2 = b.set_document(&doc_b).unwrap();
        a.apply_remote(&update2).unwrap();
        assert_eq!(a.document().unwrap(), doc_b);
        assert_eq!(a.document().unwrap(), b.document().unwrap());
    }

    #[test]
    fn full_update_bootstraps_a_fresh_peer() {
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let doc = schema_basic::doc(vec![
            schema_basic::heading(1, vec![schema_basic::text("Title", vec![]).unwrap()]).unwrap(),
            para("body"),
        ])
        .unwrap();
        a.set_document(&doc).unwrap();

        // A late joiner applies A's full-state update and sees the whole doc.
        let mut late = CollabEditor::new(7, schema_basic::schema());
        late.apply_remote(&a.full_update()).unwrap();
        assert_eq!(late.document().unwrap(), doc);
    }
}
