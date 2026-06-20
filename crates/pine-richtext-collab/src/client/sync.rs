//! The client half of the collab sync handshake — **target-agnostic**, so it is
//! host-tested here and reused by the wasm [`CollabConnection`](super::CollabConnection)
//! I/O shell.
//!
//! [`CollabSyncClient`] owns a [`CollabEditor`] and turns the y-protocols
//! handshake into editor operations. It is a *client-of-server* driver: it
//! announces its state proactively with [`hello`](Self::hello) on join, so a
//! peer's `SyncStep1` only ever needs a `SyncStep2` reply (the symmetric
//! "also send my SyncStep1" case never arises against the `CollabSync` server,
//! which volunteers its own `SyncStep1`).
//!
//! ```text
//! hello()                 -> SyncStep1(my state vector)         (sent on join)
//! on SyncStep1(remote_sv) -> reply SyncStep2(diff for remote)
//! on SyncStep2(update)    -> apply, surface the doc IF it changed
//! on Update(update)       -> apply, surface the doc IF it changed
//! push_local(node)        -> Update(diff to broadcast)
//! ```
//!
//! "IF it changed" matters: the gateway echoes a publish back to its own sender,
//! so a client receives its own `Update` again. Re-applying is a CRDT no-op, and
//! surfacing the (unchanged) document would reload the editor and reset the
//! caret on every keystroke — so [`on_message`](Self::on_message) suppresses a
//! no-op apply.

use bytes::Bytes;
use pine_richtext::RichTextResult;
use pine_richtext::model::{Node, Schema};
use pine_richtext::transform::Step;
use pocopine_collab::CollabMessage;

use crate::binder::{BindError, CollabEditor};

/// The result of handling one inbound [`CollabMessage`].
#[derive(Default)]
pub struct SyncOutcome {
    /// A message to send back over the wire (a SyncStep2 reply to a peer's
    /// SyncStep1). `None` for an applied update.
    pub reply: Option<CollabMessage>,
    /// The document after applying the message, ONLY when it actually changed —
    /// push this into the view editor. `None` for a SyncStep1 (a read) or a
    /// no-op apply (an echo / duplicate).
    pub document: Option<Node>,
}

/// Drives the collab sync handshake against a [`CollabEditor`] from the client
/// side. Pure logic (no transport); the wasm shell wires it to a `RealtimeClient`.
pub struct CollabSyncClient {
    editor: CollabEditor,
}

impl CollabSyncClient {
    /// A fresh client with a `client_id` unique across every concurrent writer
    /// of the document (see [`CollabEditor::new`](crate::CollabEditor::new) — a
    /// per-process `ws-N` counter is NOT safe; use a random 53-bit id, e.g. the
    /// `random_client_id` helper).
    pub fn new(client_id: u64, schema: Schema) -> Self {
        Self {
            editor: CollabEditor::new(client_id, schema),
        }
    }

    /// The current document as a `Node`.
    pub fn document(&self) -> RichTextResult<Node> {
        self.editor.document()
    }

    /// The SyncStep1 message to send right after the subscribe is acknowledged —
    /// "here is what I already have."
    pub fn hello(&self) -> CollabMessage {
        CollabMessage::SyncStep1(Bytes::from(self.editor.state_vector()))
    }

    /// Decode a raw frame payload and handle it. Keeping the decode here (rather
    /// than in the wasm shell) makes the whole inbound path host-testable.
    pub fn on_payload(&mut self, payload: &[u8]) -> Result<SyncOutcome, BindError> {
        let message =
            CollabMessage::decode(payload).map_err(|err| BindError::Protocol(err.to_string()))?;
        self.on_message(message)
    }

    /// Handle one inbound collab message, returning any reply to send and the
    /// updated document (only when it changed).
    pub fn on_message(&mut self, message: CollabMessage) -> Result<SyncOutcome, BindError> {
        match message {
            CollabMessage::SyncStep1(remote_sv) => {
                // A peer's state vector: answer with the diff it is missing.
                let diff = self.editor.diff(&remote_sv)?;
                Ok(SyncOutcome {
                    reply: Some(CollabMessage::SyncStep2(Bytes::from(diff))),
                    document: None,
                })
            }
            CollabMessage::SyncStep2(update) | CollabMessage::Update(update) => {
                // Catch-up diff or a live edit. Compare before/after so a no-op
                // apply (our own echo, a duplicate) does NOT surface a reload.
                // `before` may be un-decodable on a still-empty editor (an empty
                // "doc" fragment isn't a valid document) — treat that as changed.
                let before = self.editor.document().ok();
                let after = self.editor.apply_remote(&update)?;
                Ok(SyncOutcome {
                    reply: None,
                    document: (before.as_ref() != Some(&after)).then_some(after),
                })
            }
            // Ephemeral presence — handled by the connection's awareness layer,
            // never the document sync driver.
            CollabMessage::Awareness(_) => Ok(SyncOutcome {
                reply: None,
                document: None,
            }),
        }
    }

    /// Commit a local edit, coarsely (a whole-document rebuild). Prefer
    /// [`push_local_steps`](Self::push_local_steps) when the editor's transaction
    /// steps are available — it preserves peers' cursors and converges
    /// concurrent in-block edits.
    pub fn push_local(&mut self, doc: &Node) -> Result<CollabMessage, BindError> {
        self.push_local_steps(doc, &[])
    }

    /// Commit a local edit as a fine-grained incremental update from the editor's
    /// transaction `steps` (`new_doc` is the document after them), falling back to
    /// a coarse rebuild for structural steps. Returns the `Update` to broadcast.
    pub fn push_local_steps(
        &mut self,
        new_doc: &Node,
        steps: &[Step],
    ) -> Result<CollabMessage, BindError> {
        let update = self.editor.apply_local(new_doc, steps)?;
        Ok(CollabMessage::Update(Bytes::from(update)))
    }

    /// Anchor a model caret/selection endpoint so it survives concurrent edits —
    /// capture before applying a remote update, then [`resolve_point`]
    /// (Self::resolve_point) after to find where it moved.
    pub fn point_at(&self, model_pos: usize) -> RichTextResult<Option<crate::StickyPoint>> {
        self.editor.point_at(model_pos)
    }

    /// Resolve a [`StickyPoint`](crate::StickyPoint) to its current model position.
    pub fn resolve_point(&self, point: &crate::StickyPoint) -> RichTextResult<Option<usize>> {
        self.editor.point_model_pos(point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::schema_basic;

    fn para(text: &str) -> Node {
        schema_basic::paragraph(vec![schema_basic::text(text, vec![]).unwrap()]).unwrap()
    }

    fn doc(paras: Vec<Node>) -> Node {
        schema_basic::doc(paras).unwrap()
    }

    #[test]
    fn a_live_update_converges_a_peer() {
        let mut a = CollabSyncClient::new(1, schema_basic::schema());
        let mut b = CollabSyncClient::new(2, schema_basic::schema());

        let edit = a.push_local(&doc(vec![para("hello")])).unwrap();
        let CollabMessage::Update(bytes) = edit else {
            panic!("push_local yields an Update");
        };
        let out = b.on_message(CollabMessage::Update(bytes)).unwrap();

        assert!(out.reply.is_none());
        assert_eq!(out.document.unwrap(), a.document().unwrap());
    }

    #[test]
    fn fine_diff_edit_converges_and_preserves_a_caret() {
        use pine_richtext::model::{Fragment, Slice};
        use pine_richtext::transform::ReplaceStep;

        let schema = schema_basic::schema();
        let mut a = CollabSyncClient::new(1, schema_basic::schema());
        let mut b = CollabSyncClient::new(2, schema_basic::schema());

        // Both start on "hello world".
        let base = doc(vec![para("hello world")]);
        let CollabMessage::Update(init) = a.push_local(&base).unwrap() else {
            panic!("update");
        };
        b.on_message(CollabMessage::Update(init)).unwrap();

        // B holds a caret after "hello " (pos 7); A inserts "XYZ" at the start.
        let caret = b.point_at(7).unwrap().expect("caret");
        let text = schema_basic::text("XYZ", vec![]).unwrap();
        let step = Step::Replace(ReplaceStep {
            from: 1,
            to: 1,
            slice: Slice::new(Fragment::new(vec![text]), 0, 0),
            structure: false,
        });
        let new_a = step.apply(&base, &schema).unwrap().doc;
        let CollabMessage::Update(edit) = a.push_local_steps(&new_a, &[step]).unwrap() else {
            panic!("update");
        };

        // B merges A's fine-diff edit, converges, and its caret tracked the insert.
        let out = b.on_message(CollabMessage::Update(edit)).unwrap();
        assert_eq!(out.document.unwrap(), a.document().unwrap());
        assert_eq!(b.resolve_point(&caret).unwrap(), Some(10), "caret 7 -> 10");
    }

    #[test]
    fn re_applying_an_update_does_not_surface_a_reload() {
        // Models the gateway echoing a publish back to its own sender: the
        // second apply is a no-op and must NOT yield a document (which would
        // reload the editor and reset the caret).
        let mut a = CollabSyncClient::new(1, schema_basic::schema());
        let mut b = CollabSyncClient::new(2, schema_basic::schema());
        let CollabMessage::Update(bytes) = a.push_local(&doc(vec![para("hi")])).unwrap() else {
            panic!("update");
        };

        let first = b.on_message(CollabMessage::Update(bytes.clone())).unwrap();
        assert!(first.document.is_some(), "first apply changes the doc");

        let echo = b.on_message(CollabMessage::Update(bytes)).unwrap();
        assert!(
            echo.document.is_none(),
            "re-applying the same update is a no-op"
        );
    }

    #[test]
    fn handshake_against_a_server_role() {
        // Model the real client<->server flow, not peer-to-peer: the "server"
        // (another driver standing in) volunteers SyncStep2 + its own SyncStep1.
        let mut server = CollabSyncClient::new(1, schema_basic::schema());
        server.push_local(&doc(vec![para("shared")])).unwrap();

        let mut client = CollabSyncClient::new(2, schema_basic::schema());

        // Client joins: SyncStep1 -> server answers SyncStep2 (the catch-up).
        let hello = client.hello();
        let server_reply = server.on_message(hello).unwrap();
        let step2 = server_reply.reply.expect("server replies SyncStep2");
        let caught_up = client.on_message(step2).unwrap();
        assert_eq!(caught_up.document.unwrap(), server.document().unwrap());

        // Server also volunteers its own SyncStep1; the client answers SyncStep2
        // (uploading anything the server lacks — here nothing, so it applies clean).
        let server_step1 = server.hello();
        let client_reply = client.on_message(server_step1).unwrap();
        assert!(
            client_reply.reply.is_some(),
            "client answers the server's SyncStep1 with SyncStep2"
        );
        assert!(client_reply.document.is_none());
    }

    #[test]
    fn on_payload_rejects_a_malformed_message() {
        let mut c = CollabSyncClient::new(1, schema_basic::schema());
        assert!(matches!(c.on_payload(&[]), Err(BindError::Protocol(_))));
        assert!(matches!(
            c.on_payload(&[9, 1, 2]),
            Err(BindError::Protocol(_))
        ));
    }
}
