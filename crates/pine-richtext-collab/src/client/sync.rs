//! The client half of the collab sync handshake — **target-agnostic**, so it is
//! host-tested here and reused by the wasm [`CollabConnection`](super::CollabConnection)
//! I/O shell.
//!
//! [`CollabSyncClient`] owns a [`CollabEditor`] and turns the y-protocols
//! handshake into editor operations. It is a *client-of-server* driver: it
//! announces its compatibility identity and state proactively with
//! [`hello`](Self::hello) on join. The server returns its own hello before any
//! yrs update, then each requested `SyncStep2` flows in the proper direction.
//!
//! ```text
//! hello()                 -> Hello(identity, my state vector)   (sent on join)
//! on compatible Hello     -> optional SyncStep2(diff for remote)
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
use pine_richtext::model::Node;
use pine_richtext::runtime::EditorRuntime;
use pine_richtext::transform::Step;
use pocopine_collab::{CollabHello, CollabMessage, CompatibilityIdentity};

use crate::binder::{BindError, CollabEditor};
use crate::runtime_compatibility;

/// The result of handling one inbound [`CollabMessage`].
#[derive(Default)]
pub struct SyncOutcome {
    /// A message to send back over the wire (a requested SyncStep2 reply to a
    /// compatible hello). `None` for an applied update.
    pub reply: Option<CollabMessage>,
    /// The document after applying the message, ONLY when it actually changed —
    /// push this into the view editor. `None` for a hello (a read) or a
    /// no-op apply (an echo / duplicate).
    pub document: Option<Node>,
}

/// Drives the collab sync handshake against a [`CollabEditor`] from the client
/// side. Pure logic (no transport); the wasm shell wires it to a `RealtimeClient`.
pub struct CollabSyncClient {
    editor: CollabEditor,
    compatibility: CompatibilityIdentity,
    peer_compatible: bool,
}

impl CollabSyncClient {
    /// A fresh client with a `client_id` unique across every concurrent writer
    /// of the document (see [`CollabEditor::new`](crate::CollabEditor::new) — a
    /// per-process `ws-N` counter is NOT safe; use a random 53-bit id, e.g. the
    /// `random_client_id` helper).
    pub fn new(client_id: u64, runtime: &EditorRuntime) -> Self {
        Self {
            editor: CollabEditor::new(client_id, runtime.schema().clone()),
            compatibility: runtime_compatibility(runtime),
            peer_compatible: false,
        }
    }

    /// The current document as a `Node`.
    pub fn document(&self) -> RichTextResult<Node> {
        self.editor.document()
    }

    /// The compatibility hello to send after subscribe is acknowledged.
    pub fn hello(&mut self) -> CollabMessage {
        // A reconnect/re-subscribe is a fresh negotiation. Nothing received on
        // the old subscription authorizes updates on the new one.
        self.peer_compatible = false;
        CollabMessage::Hello(CollabHello::new(
            self.compatibility.clone(),
            Bytes::from(self.editor.state_vector()),
            true,
        ))
    }

    /// Whether a matching peer hello has been validated for this subscription.
    pub fn is_compatible(&self) -> bool {
        self.peer_compatible
    }

    /// Decode a raw frame payload and handle it. Keeping the decode here (rather
    /// than in the wasm shell) makes the whole inbound path host-testable.
    pub fn on_payload(&mut self, payload: &[u8]) -> Result<SyncOutcome, BindError> {
        let message = match CollabMessage::decode(payload) {
            Ok(message) => message,
            Err(err) => {
                self.peer_compatible = false;
                return Err(BindError::Protocol(err.to_string()));
            }
        };
        self.on_message(message)
    }

    /// Handle one inbound collab message, returning any reply to send and the
    /// updated document (only when it changed).
    pub fn on_message(&mut self, message: CollabMessage) -> Result<SyncOutcome, BindError> {
        match message {
            CollabMessage::Hello(hello) => {
                // Repeated hellos restart negotiation. Decode the state vector
                // before recording success, so malformed hello state is closed.
                self.peer_compatible = false;
                if hello.compatibility() != &self.compatibility {
                    return Err(BindError::Protocol(format!(
                        "collab compatibility mismatch: peer v{}:{}, local v{}:{}",
                        hello.compatibility().protocol_version(),
                        hello.compatibility().fingerprint(),
                        self.compatibility.protocol_version(),
                        self.compatibility.fingerprint()
                    )));
                }
                let diff = self.editor.diff(hello.state_vector())?;
                self.peer_compatible = true;
                Ok(SyncOutcome {
                    reply: hello
                        .requests_sync_step2()
                        .then(|| CollabMessage::SyncStep2(Bytes::from(diff))),
                    document: None,
                })
            }
            CollabMessage::SyncStep2(update) | CollabMessage::Update(update) => {
                if !self.peer_compatible {
                    return Err(BindError::Protocol(
                        "collab update received before a compatible hello".into(),
                    ));
                }
                // Catch-up diff or a live edit. Compare before/after so a no-op
                // apply (our own echo, a duplicate) does NOT surface a reload.
                // `before` may be un-decodable on a still-empty editor (an empty
                // "doc" fragment isn't a valid document) — treat that as changed.
                let before = self.editor.document_if_initialized()?;
                let after = self.editor.apply_remote_if_initialized(&update)?;
                Ok(SyncOutcome {
                    reply: None,
                    document: after.filter(|after| before.as_ref() != Some(after)),
                })
            }
            // Ephemeral presence — handled by the connection's awareness layer,
            // never the document sync driver.
            CollabMessage::Awareness(_) => {
                if !self.peer_compatible {
                    return Err(BindError::Protocol(
                        "collab awareness received before a compatible hello".into(),
                    ));
                }
                Ok(SyncOutcome {
                    reply: None,
                    document: None,
                })
            }
        }
    }

    /// Commit a local edit, coarsely (a whole-document rebuild). Prefer
    /// [`push_local_steps`](Self::push_local_steps) when the editor's transaction
    /// steps are available — it preserves peers' cursors and converges
    /// concurrent in-block edits.
    pub fn push_local(&mut self, doc: &Node) -> Result<Option<CollabMessage>, BindError> {
        self.push_local_steps(doc, &[])
    }

    /// Commit a local edit as a fine-grained incremental update from the editor's
    /// transaction `steps` (`new_doc` is the document after them), falling back to
    /// a coarse rebuild for structural steps. Returns the `Update` to broadcast.
    pub fn push_local_steps(
        &mut self,
        new_doc: &Node,
        steps: &[Step],
    ) -> Result<Option<CollabMessage>, BindError> {
        let update = self.editor.apply_local(new_doc, steps)?;
        Ok(self
            .peer_compatible
            .then(|| CollabMessage::Update(Bytes::from(update))))
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
    use pine_richtext::runtime::RuntimeBuilder;
    use pine_richtext::schema_basic;

    fn para(text: &str) -> Node {
        schema_basic::paragraph(vec![schema_basic::text(text, vec![]).unwrap()]).unwrap()
    }

    fn doc(paras: Vec<Node>) -> Node {
        schema_basic::doc(paras).unwrap()
    }

    fn client(client_id: u64) -> CollabSyncClient {
        CollabSyncClient::new(client_id, &RuntimeBuilder::new().build())
    }

    /// Exchange both hellos before either side handles one, then deliver the
    /// requested diffs in their proper direction. Both drivers finish verified.
    fn negotiate(a: &mut CollabSyncClient, b: &mut CollabSyncClient) {
        let hello_a = a.hello();
        let hello_b = b.hello();
        let to_b = a.on_message(hello_b).unwrap().reply;
        let to_a = b.on_message(hello_a).unwrap().reply;
        if let Some(message) = to_b {
            b.on_message(message).unwrap();
        }
        if let Some(message) = to_a {
            a.on_message(message).unwrap();
        }
        assert!(a.is_compatible() && b.is_compatible());
    }

    #[test]
    fn a_live_update_converges_a_peer() {
        let mut a = client(1);
        let mut b = client(2);
        negotiate(&mut a, &mut b);

        let edit = a
            .push_local(&doc(vec![para("hello")]))
            .unwrap()
            .expect("verified peer gets an update");
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
        let mut a = client(1);
        let mut b = client(2);
        negotiate(&mut a, &mut b);

        // Both start on "hello world".
        let base = doc(vec![para("hello world")]);
        let CollabMessage::Update(init) = a.push_local(&base).unwrap().unwrap() else {
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
        let CollabMessage::Update(edit) = a.push_local_steps(&new_a, &[step]).unwrap().unwrap()
        else {
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
        let mut a = client(1);
        let mut b = client(2);
        negotiate(&mut a, &mut b);
        let CollabMessage::Update(bytes) = a.push_local(&doc(vec![para("hi")])).unwrap().unwrap()
        else {
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
        // (another driver standing in) exchanges a hello before SyncStep2.
        let mut server = client(1);
        assert!(
            server
                .push_local(&doc(vec![para("shared")]))
                .unwrap()
                .is_none(),
            "a pre-hello edit stays local"
        );

        let mut client = client(2);

        // Both roles send their compatibility hello before processing replies.
        let client_hello = client.hello();
        let server_hello = server.hello();
        let server_reply = server.on_message(client_hello).unwrap();
        let client_reply = client.on_message(server_hello).unwrap();

        let step2 = server_reply.reply.expect("server replies SyncStep2");
        let caught_up = client.on_message(step2).unwrap();
        assert_eq!(caught_up.document.unwrap(), server.document().unwrap());

        assert!(
            client_reply.reply.is_some(),
            "client answers the server hello with SyncStep2"
        );
        assert!(client_reply.document.is_none());
    }

    #[test]
    fn on_payload_rejects_a_malformed_message() {
        let mut c = client(1);
        assert!(matches!(c.on_payload(&[]), Err(BindError::Protocol(_))));
        assert!(matches!(
            c.on_payload(&[9, 1, 2]),
            Err(BindError::Protocol(_))
        ));
    }

    #[test]
    fn pre_hello_and_mismatched_peers_exchange_no_yrs_updates() {
        let mut local = client(1);
        let local_doc = doc(vec![para("local")]);
        assert!(local.push_local(&local_doc).unwrap().is_none());

        let mut remote = client(2);
        let remote_update = remote
            .editor
            .apply_local(&doc(vec![para("remote")]), &[])
            .unwrap();

        assert!(matches!(
            local.on_message(CollabMessage::Update(remote_update.clone().into())),
            Err(BindError::Protocol(_))
        ));
        assert_eq!(local.document().unwrap(), local_doc);

        let mismatched = CompatibilityIdentity::new(
            crate::PINE_COLLAB_PROTOCOL_VERSION,
            "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        )
        .unwrap();
        let hello = CollabMessage::Hello(CollabHello::new(
            mismatched,
            remote.editor.state_vector(),
            true,
        ));
        assert!(matches!(
            local.on_message(hello),
            Err(BindError::Protocol(_))
        ));
        assert!(!local.is_compatible());

        // A valid yrs update after the rejected hello is still refused and the
        // local document is byte-for-byte semantically unchanged.
        assert!(matches!(
            local.on_message(CollabMessage::Update(remote_update.into())),
            Err(BindError::Protocol(_))
        ));
        assert_eq!(local.document().unwrap(), local_doc);
    }

    #[test]
    fn malformed_repeated_hello_revokes_an_accepted_peer() {
        let mut a = client(1);
        let mut b = client(2);
        negotiate(&mut a, &mut b);

        assert!(matches!(a.on_payload(&[0]), Err(BindError::Protocol(_))));
        assert!(!a.is_compatible());
        assert!(matches!(
            a.on_message(CollabMessage::Update(Bytes::new())),
            Err(BindError::Protocol(_))
        ));
    }
}
