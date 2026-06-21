//! The live binding: [`CollabEditor`] syncs a pine-richtext document `Node`
//! with a yrs CRDT `Doc` so edits converge across replicas over the
//! `pocopine-collab` transport.
//!
//! ## Two write paths
//!
//! [`CollabEditor::set_document`] re-encodes the whole document (a **coarse**
//! write): correct for one author, but it tombstones the tree on every edit, so
//! two writers fight and `StickyIndex` anchors are destroyed.
//!
//! [`CollabEditor::apply_local`] is the **fine-grained** (Phase 5) write: it maps
//! a pine-richtext `Step` directly to a targeted yrs mutation (see [`step_writer`]
//! (crate::step_writer)), so an in-block edit touches only that block's `XmlText`
//! and concurrent edits to different spans converge without loss. Structural steps
//! (block split/merge/move, lists, attrs) fall back to a coarse rebuild — they
//! carry permanent concurrent-loss anyway (schema-A, design doc §6). This is what
//! unblocks multi-writer co-editing and surviving `StickyIndex` cursors.
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
use pine_richtext::transform::Step;
use pine_richtext::{RichTextError, RichTextResult};
use yrs::types::xml::{XmlFragment, XmlFragmentRef};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
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
    /// A collab wire message could not be decoded (bad tag / empty body).
    Protocol(String),
    /// The realtime transport failed to connect (bad URL / WebSocket error).
    Connect(String),
    /// A second editor was bound to a connection that already drives one. A
    /// `CollabConnection` drives exactly one editor; open a separate connection
    /// per editor.
    AlreadyBound,
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
            Self::Protocol(msg) => write!(f, "collab protocol error: {msg}"),
            Self::Connect(msg) => write!(f, "collab connect error: {msg}"),
            Self::AlreadyBound => write!(
                f,
                "collab connection already drives an editor; open a separate connection per editor"
            ),
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
    /// A new, empty editor with a caller-supplied `client_id`.
    ///
    /// `client_id` MUST be unique across **every concurrent writer of this
    /// document**, including writers on *other* server processes. A per-process
    /// counter such as the realtime `ws-N` session number is NOT safe: two web
    /// replicas both start numbering at `ws-1`, so two distinct writers would
    /// share one yrs client id and silently corrupt the CRDT. Use a random
    /// 53-bit integer instead (on wasm the `random_client_id` helper mints one
    /// RNG-free, no `getrandom`).
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
        self.rebuild(doc)?;
        Ok(self.doc.transact().encode_diff_v1(&before))
    }

    /// Coarse rebuild: tombstone the whole tree and re-encode `doc`. Shared by
    /// [`set_document`](Self::set_document) and the [`apply_local`](Self::apply_local)
    /// structural fallback.
    fn rebuild(&mut self, doc: &Node) -> RichTextResult<()> {
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
        Ok(())
    }

    /// Commit a local edit as a **fine-grained incremental** update when every
    /// step is an in-block inline edit (typing, deleting, marks), falling back to
    /// a coarse rebuild for structural steps (block split/merge/move, lists,
    /// attrs). `new_doc` is the document *after* `steps`; returns the yrs update
    /// to broadcast — the diff since the prior state.
    ///
    /// This is the Phase 5 write path: unlike [`set_document`](Self::set_document)
    /// it does not tombstone the tree on an ordinary edit, so two writers editing
    /// different spans converge without loss and `StickyIndex` cursors survive.
    pub fn apply_local(&mut self, new_doc: &Node, steps: &[Step]) -> RichTextResult<Vec<u8>> {
        let before = self.doc.transact().state_vector();
        let mut coarse = steps.is_empty();
        if !coarse {
            // Resolve each step against the model as of just before it; keep the
            // model in lockstep so positions stay correct across the batch.
            let mut current = self.document()?;
            let mut txn = self.doc.transact_mut_with(ORIGIN_LOCAL.to_string());
            for step in steps {
                if crate::step_writer::try_apply_fine(
                    &mut txn,
                    &self.root,
                    &current,
                    &self.schema,
                    step,
                )? {
                    current = step.apply(&current, &self.schema)?.doc;
                } else {
                    coarse = true;
                    break;
                }
            }
        }
        if coarse {
            // A partial fine application above is harmless: the rebuild tombstones
            // it and re-encodes `new_doc`, so the final state is correct either way.
            self.rebuild(new_doc)?;
        }
        Ok(self.doc.transact().encode_diff_v1(&before))
    }

    /// Anchor an absolute model position to a [`StickyPoint`] that tracks the
    /// document through concurrent edits — used to preserve a local caret across a
    /// remote edit, or to publish a live remote cursor. `None` outside a flat
    /// textblock (block boundaries / nested blocks aren't anchored).
    pub fn point_at(&self, model_pos: usize) -> RichTextResult<Option<crate::StickyPoint>> {
        let txn = self.doc.transact();
        let doc = decode_doc(&txn, &self.root, &self.schema)?;
        Ok(crate::caret::point_at(
            &txn,
            &self.root,
            &doc,
            &self.schema,
            model_pos,
        ))
    }

    /// Resolve a [`StickyPoint`] back to an absolute model position in the current
    /// document, or `None` if its block is gone (a coarse rebuild re-minted ids).
    pub fn point_model_pos(&self, point: &crate::StickyPoint) -> RichTextResult<Option<usize>> {
        let txn = self.doc.transact();
        let doc = decode_doc(&txn, &self.root, &self.schema)?;
        Ok(crate::caret::point_model_pos(&txn, &self.root, &doc, point))
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

    /// This editor's state vector — "what I already have" — encoded as the
    /// SyncStep1 payload of the collab handshake.
    pub fn state_vector(&self) -> Vec<u8> {
        self.doc.transact().state_vector().encode_v1()
    }

    /// The update a peer whose state vector is `remote_sv` is missing (the
    /// SyncStep2 reply). With an empty `remote_sv` this equals
    /// [`full_update`](Self::full_update).
    pub fn diff(&self, remote_sv: &[u8]) -> Result<Vec<u8>, BindError> {
        let sv =
            StateVector::decode_v1(remote_sv).map_err(|err| BindError::Crdt(err.to_string()))?;
        Ok(self.doc.transact().encode_state_as_update_v1(&sv))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::model::{Fragment, Slice};
    use pine_richtext::schema_basic;
    use pine_richtext::transform::{MarkStep, ReplaceStep};

    fn para(text: &str) -> Node {
        schema_basic::paragraph(vec![schema_basic::text(text, vec![]).unwrap()]).unwrap()
    }

    /// Build an inline text-insertion step at model position `pos` and the
    /// document that results from applying it.
    fn insert_step(doc: &Node, schema: &Schema, pos: usize, text: &str) -> (Step, Node) {
        let node = schema_basic::text(text, vec![]).unwrap();
        let slice = Slice::new(Fragment::new(vec![node]), 0, 0);
        let step = Step::Replace(ReplaceStep {
            from: pos,
            to: pos,
            slice,
            structure: false,
        });
        let new_doc = step.apply(doc, schema).unwrap().doc;
        (step, new_doc)
    }

    #[test]
    fn apply_local_inserts_text_incrementally_and_converges() {
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // Insert "X" at the start of the paragraph's content (model pos 1).
        let (step, new_doc) = insert_step(&doc0, &schema, 1, "X");
        let update = a.apply_local(&new_doc, &[step]).unwrap();
        assert_eq!(a.document().unwrap(), new_doc); // == para("Xhello")

        b.apply_remote(&update).unwrap();
        assert_eq!(b.document().unwrap(), new_doc);
        assert_eq!(a.document().unwrap(), b.document().unwrap());
    }

    #[test]
    fn concurrent_in_block_edits_both_survive() {
        // The headline: two writers editing different spans of the SAME block
        // converge with BOTH edits. The coarse write would tombstone the block on
        // each side and the merge would duplicate/clobber — this asserts otherwise.
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello world")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // A prepends "A" (pos 1); B appends "B" (pos 12 — end of the 11-char body).
        let (step_a, new_a) = insert_step(&doc0, &schema, 1, "A");
        let (step_b, new_b) = insert_step(&doc0, &schema, 12, "B");
        let ua = a.apply_local(&new_a, &[step_a]).unwrap();
        let ub = b.apply_local(&new_b, &[step_b]).unwrap();

        // Cross-apply; both converge to a doc carrying BOTH insertions.
        a.apply_remote(&ub).unwrap();
        b.apply_remote(&ua).unwrap();
        let expected = schema_basic::doc(vec![para("Ahello worldB")]).unwrap();
        assert_eq!(a.document().unwrap(), expected);
        assert_eq!(b.document().unwrap(), expected);
    }

    #[test]
    fn apply_local_deletes_text_incrementally_and_converges() {
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // Delete the leading 'h' (range [1,2) with an empty slice).
        let slice = Slice::new(Fragment::new(vec![]), 0, 0);
        let step = Step::Replace(ReplaceStep {
            from: 1,
            to: 2,
            slice,
            structure: false,
        });
        let new_doc = step.apply(&doc0, &schema).unwrap().doc; // para("ello")
        let update = a.apply_local(&new_doc, &[step]).unwrap();
        assert_eq!(a.document().unwrap(), new_doc);
        b.apply_remote(&update).unwrap();
        assert_eq!(b.document().unwrap(), new_doc);
    }

    #[test]
    fn apply_local_adds_a_mark_incrementally_and_converges() {
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // Bold "hell" (model range [1,5)).
        let step = Step::AddMark(MarkStep {
            from: 1,
            to: 5,
            mark: schema_basic::strong().unwrap(),
        });
        let new_doc = step.apply(&doc0, &schema).unwrap().doc;
        let update = a.apply_local(&new_doc, &[step]).unwrap();
        assert_eq!(a.document().unwrap(), new_doc, "local doc carries the mark");
        b.apply_remote(&update).unwrap();
        assert_eq!(
            b.document().unwrap(),
            new_doc,
            "peer converges with the mark"
        );
    }

    #[test]
    fn apply_local_applies_a_multi_step_batch() {
        // Two steps in one commit: the second must resolve against the model as
        // updated by the first (position lockstep).
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        let (s1, d1) = insert_step(&doc0, &schema, 1, "X"); // "Xhello"
        let (s2, d2) = insert_step(&d1, &schema, 2, "Y"); // "XYhello"
        let update = a.apply_local(&d2, &[s1, s2]).unwrap();
        assert_eq!(a.document().unwrap(), d2);
        b.apply_remote(&update).unwrap();
        assert_eq!(b.document().unwrap(), d2);
    }

    #[test]
    fn apply_local_removes_a_mark_incrementally_and_converges() {
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        // Start with "hell" bolded + "o" plain.
        let bolded = schema_basic::doc(vec![
            schema_basic::paragraph(vec![
                schema_basic::text("hell", vec![schema_basic::strong().unwrap()]).unwrap(),
                schema_basic::text("o", vec![]).unwrap(),
            ])
            .unwrap(),
        ])
        .unwrap();
        b.apply_remote(&a.set_document(&bolded).unwrap()).unwrap();

        // Remove strong over [1,5) → the whole paragraph becomes plain "hello".
        let step = Step::RemoveMark(MarkStep {
            from: 1,
            to: 5,
            mark: schema_basic::strong().unwrap(),
        });
        let plain = step.apply(&bolded, &schema).unwrap().doc;
        let update = a.apply_local(&plain, &[step]).unwrap();
        assert_eq!(a.document().unwrap(), plain, "mark cleared locally");
        b.apply_remote(&update).unwrap();
        assert_eq!(b.document().unwrap(), plain, "peer converges unmarked");
    }

    /// A tiny deterministic xorshift PRNG, so a fuzz failure reproduces exactly.
    fn rng_next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    /// A random in-block edit (insert a char, or delete one) against the single
    /// paragraph of `base`, plus the resulting document.
    fn random_edit(base: &Node, schema: &Schema, rng: &mut u64) -> (Step, Node) {
        let content_size = base.child(0).unwrap().content_size();
        if content_size > 0 && rng_next(rng).is_multiple_of(3) {
            // Delete one char at a random in-content offset.
            let from = 1 + (rng_next(rng) as usize % content_size);
            let step = Step::Replace(ReplaceStep {
                from,
                to: from + 1,
                slice: Slice::new(Fragment::new(vec![]), 0, 0),
                structure: false,
            });
            let new_doc = step.apply(base, schema).unwrap().doc;
            (step, new_doc)
        } else {
            // Insert a char at a random position in the content range [1, size+1].
            let pos = 1 + (rng_next(rng) as usize % (content_size + 1));
            // Mix multi-byte chars (é = 2 bytes, 🎉 = 4) so the fuzz exercises the
            // char↔byte offset conversion, not just ASCII.
            let alphabet = ['a', 'b', 'é', '🎉', 'z'];
            let ch = alphabet[rng_next(rng) as usize % alphabet.len()].to_string();
            insert_step(base, schema, pos, &ch)
        }
    }

    #[test]
    fn fuzz_concurrent_in_block_edits_always_converge() {
        // A4 convergence gate: 100 rounds of two writers each making an
        // independent in-block edit off the shared state, then exchanging — the
        // two replicas must reconcile to the same document every round.
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let start = schema_basic::doc(vec![para("the quick brown fox")]).unwrap();
        b.apply_remote(&a.set_document(&start).unwrap()).unwrap();

        let mut rng = 0x2545_F491_4F6C_DD1D_u64;
        for round in 0..100 {
            let base = a.document().unwrap();
            assert_eq!(
                base,
                b.document().unwrap(),
                "synced at the top of round {round}"
            );

            let (sa, da) = random_edit(&base, &schema, &mut rng);
            let (sb, db) = random_edit(&base, &schema, &mut rng);
            let ua = a.apply_local(&da, &[sa]).unwrap();
            let ub = b.apply_local(&db, &[sb]).unwrap();

            a.apply_remote(&ub).unwrap();
            b.apply_remote(&ua).unwrap();
            assert_eq!(
                a.document().unwrap(),
                b.document().unwrap(),
                "diverged after round {round}"
            );
        }
    }

    #[test]
    fn concurrent_conflicting_marks_converge() {
        // Two writers apply a link with DIFFERENT hrefs to the same range. yrs
        // last-write-wins on the format value + deterministic normalize-on-read
        // must reconcile both replicas (design §6).
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        let link_a = Step::AddMark(MarkStep {
            from: 1,
            to: 6,
            mark: schema_basic::link("https://a.example", None::<String>).unwrap(),
        });
        let link_b = Step::AddMark(MarkStep {
            from: 1,
            to: 6,
            mark: schema_basic::link("https://b.example", None::<String>).unwrap(),
        });
        let da = link_a.apply(&doc0, &schema).unwrap().doc;
        let db = link_b.apply(&doc0, &schema).unwrap().doc;
        let ua = a.apply_local(&da, &[link_a]).unwrap();
        let ub = b.apply_local(&db, &[link_b]).unwrap();

        a.apply_remote(&ub).unwrap();
        b.apply_remote(&ua).unwrap();
        // One href wins (LWW), but both replicas agree on which.
        assert_eq!(a.document().unwrap(), b.document().unwrap());
    }

    #[test]
    fn codec_round_trips_a_mark_over_an_emoji() {
        // The coarse codec's format-range write must also use byte offsets: bold
        // "a👍" then plain "b". A char-offset format range would mis-cover the
        // multi-byte emoji.
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![
                schema_basic::text("a👍", vec![schema_basic::strong().unwrap()]).unwrap(),
                schema_basic::text("b", vec![]).unwrap(),
            ])
            .unwrap(),
        ])
        .unwrap();
        b.apply_remote(&a.set_document(&doc).unwrap()).unwrap();
        assert_eq!(a.document().unwrap(), doc, "encode/decode round-trips");
        assert_eq!(b.document().unwrap(), doc, "and converges to a peer");
    }

    #[test]
    fn fine_diff_handles_multibyte_text() {
        // Regression for the char-vs-byte offset bug: yrs XmlText indexes in UTF-8
        // bytes. An edit positioned by a char offset would land mid-emoji (panic or
        // corruption). "a👍b" — insert "X" before 'b' (model pos 3, after the emoji).
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("a👍b")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        let (step, new_doc) = insert_step(&doc0, &schema, 3, "X"); // "a👍Xb"
        let update = a.apply_local(&new_doc, &[step]).unwrap();
        assert_eq!(a.document().unwrap(), new_doc);
        b.apply_remote(&update).unwrap();
        assert_eq!(a.document().unwrap(), b.document().unwrap());
    }

    #[test]
    fn fine_diff_deletes_and_marks_across_an_emoji() {
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("x🎉yz")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // Delete the emoji (char range [2,3)).
        let del = Step::Replace(ReplaceStep {
            from: 2,
            to: 3,
            slice: Slice::new(Fragment::new(vec![]), 0, 0),
            structure: false,
        });
        let after_del = del.apply(&doc0, &schema).unwrap().doc; // "xyz"
        a.apply_remote(&b.apply_local(&after_del, &[del]).unwrap())
            .unwrap();
        assert_eq!(a.document().unwrap(), after_del);

        // Bold across what is now "xyz" (range [1,4)).
        let mark = Step::AddMark(MarkStep {
            from: 1,
            to: 4,
            mark: schema_basic::strong().unwrap(),
        });
        let bolded = mark.apply(&after_del, &schema).unwrap().doc;
        b.apply_remote(&a.apply_local(&bolded, &[mark]).unwrap())
            .unwrap();
        assert_eq!(a.document().unwrap(), bolded);
        assert_eq!(a.document().unwrap(), b.document().unwrap());
    }

    #[test]
    fn caret_after_an_emoji_survives_a_remote_insert() {
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("👍hello")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // Caret after the emoji + "he" (model pos 4: emoji=1 char, h=2, e=3, |=4).
        let caret = a.point_at(4).unwrap().expect("caret");
        assert_eq!(a.point_model_pos(&caret).unwrap(), Some(4));

        // B prepends "Z" before the emoji (pos 1); A's caret shifts 4 -> 5.
        let (step, new_b) = insert_step(&doc0, &schema, 1, "Z");
        a.apply_remote(&b.apply_local(&new_b, &[step]).unwrap())
            .unwrap();
        assert_eq!(a.point_model_pos(&caret).unwrap(), Some(5));
    }

    #[test]
    fn caret_survives_a_remote_in_block_insert() {
        // A2: a StickyPoint caret tracks the document through a peer's fine-diff
        // edit. (This only works because A1 preserves the items it anchors to.)
        let schema = schema_basic::schema();
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello world")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // A anchors a caret after "hello " (model pos 7).
        let caret = a.point_at(7).unwrap().expect("caret in a textblock");
        assert_eq!(a.point_model_pos(&caret).unwrap(), Some(7), "round-trips");

        // B inserts "XYZ" at the start (pos 1) via fine-diff; A merges it.
        let (step, new_b) = insert_step(&doc0, &schema, 1, "XYZ");
        a.apply_remote(&b.apply_local(&new_b, &[step]).unwrap())
            .unwrap();

        // A's caret tracked the 3-char insertion before it: 7 -> 10.
        assert_eq!(a.point_model_pos(&caret).unwrap(), Some(10));
    }

    #[test]
    fn caret_is_lost_after_a_coarse_rebuild() {
        // The documented limitation: a coarse write re-mints block_ids, so a
        // StickyPoint can no longer find its block (the caller re-derives it).
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello")]).unwrap();
        a.set_document(&doc0).unwrap();
        let caret = a.point_at(3).unwrap().expect("caret");

        a.set_document(&schema_basic::doc(vec![para("hello")]).unwrap())
            .unwrap();
        assert_eq!(
            a.point_model_pos(&caret).unwrap(),
            None,
            "block id re-minted"
        );
    }

    #[test]
    fn structural_change_falls_back_to_coarse_and_converges() {
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let mut b = CollabEditor::new(2, schema_basic::schema());
        let doc0 = schema_basic::doc(vec![para("hello")]).unwrap();
        b.apply_remote(&a.set_document(&doc0).unwrap()).unwrap();

        // No steps but a changed doc → the coarse rebuild path; still converges.
        let doc2 = schema_basic::doc(vec![para("hello"), para("world")]).unwrap();
        let update = a.apply_local(&doc2, &[]).unwrap();
        assert_eq!(a.document().unwrap(), doc2);
        b.apply_remote(&update).unwrap();
        assert_eq!(b.document().unwrap(), doc2);
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
    fn fresh_peer_catches_up_via_the_sync_handshake() {
        // A authors a document.
        let mut a = CollabEditor::new(1, schema_basic::schema());
        let doc = schema_basic::doc(vec![para("shared")]).unwrap();
        a.set_document(&doc).unwrap();

        // A fresh peer runs SyncStep1 (its empty state vector); A answers with
        // the SyncStep2 diff; the peer applies it and converges.
        let mut fresh = CollabEditor::new(2, schema_basic::schema());
        let step2 = a.diff(&fresh.state_vector()).unwrap();
        let got = fresh.apply_remote(&step2).unwrap();
        assert_eq!(got, doc);
        assert_eq!(fresh.document().unwrap(), a.document().unwrap());
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
