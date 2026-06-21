//! The wasm WebSocket connection — the browser-side I/O shell wiring a
//! [`CollabSyncClient`] to a `pocopine-realtime` `RealtimeClient`.
//!
//! ```text
//! open(url, topic) ─ connect ─ subscribe(topic, COLLAB_SUBPROTOCOL)
//!   on Subscribed  ─► send SyncStep1 (hello)
//!   on Data        ─► driver.on_payload ─► send reply + on_change(new doc)
//!   push_local     ─► driver.push_local ─► send Update
//! ```
//!
//! The realtime callbacks capture a `Weak<RealtimeClient>` (not a strong `Rc`)
//! so the connection has no reference cycle and tears down cleanly when dropped.
//! `RealtimeClient` queues outbound frames until the socket is OPEN and the
//! topic_ref is bound, so the synchronous `subscribe` here is safe.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use pine_richtext::model::{Node, Schema};
use pocopine_collab::COLLAB_SUBPROTOCOL;
use pocopine_realtime::client::{RealtimeClient, SessionEvent};

use super::sync::CollabSyncClient;
use crate::BindError;

/// `Rc` so the data callback can clone the handler out and release the borrow
/// before invoking it (the handler may re-enter the connection).
type ChangeHandler = Rc<RefCell<Option<Rc<dyn Fn(&Node)>>>>;

/// Mint a yrs `client_id` for a browser session: a random 53-bit integer
/// (`Math.random`-derived), RNG-free so it needs no `getrandom` on wasm.
///
/// This is the id to pass to [`CollabConnection::open`] / [`CollabEditor::new`]
/// (crate::CollabEditor::new). Unlike a per-process `ws-N` session counter it
/// never collides across server replicas — two tabs, wherever they connect, get
/// distinct ids — so concurrent writers never share a yrs client id and the CRDT
/// stays correct. Collisions are astronomically unlikely (birthday bound over
/// 2^53, far beyond any realistic peer count for one document).
pub fn random_client_id() -> u64 {
    // 2^53 - 1 — the largest integer `Math.random()` can address exactly.
    (js_sys::Math::random() * 9_007_199_254_740_991.0) as u64
}

/// A live collaborative rich-text session over the realtime gateway.
///
/// # Multi-writer
///
/// [`push_local_steps`](Self::push_local_steps) (and [`bind`](Self::bind), which
/// wires it to an editor) maps the editor's transaction steps to a *fine-grained*
/// CRDT write, so concurrent in-block edits converge without loss. The legacy
/// [`push_local`](Self::push_local) re-encodes the whole document (coarse) and is
/// kept only for callers without step information — two coarse writers still
/// clobber each other.
pub struct CollabConnection {
    client: Rc<RealtimeClient>,
    driver: Rc<RefCell<CollabSyncClient>>,
    topic: String,
    on_change: ChangeHandler,
}

impl CollabConnection {
    /// Open a collaborative session: connect to `url`, subscribe to `topic`
    /// under the collab sub-protocol, and run the sync handshake. `client_id`
    /// must be unique across every concurrent writer of the document, including
    /// writers on other server processes — pass [`random_client_id`], NOT a
    /// per-process `ws-N` session number (which collides across replicas; see
    /// [`CollabEditor::new`](crate::CollabEditor::new)).
    pub fn open(
        url: &str,
        topic: impl Into<String>,
        client_id: u64,
        schema: Schema,
    ) -> Result<Self, BindError> {
        let topic = topic.into();
        let client = Rc::new(
            RealtimeClient::connect(url).map_err(|err| BindError::Connect(format!("{err:?}")))?,
        );
        let driver = Rc::new(RefCell::new(CollabSyncClient::new(client_id, schema)));
        let on_change: ChangeHandler = Rc::new(RefCell::new(None));

        // Once our subscribe is acked (the topic_ref is bound), send SyncStep1.
        {
            let weak: Weak<RealtimeClient> = Rc::downgrade(&client);
            let driver = driver.clone();
            let topic_ev = topic.clone();
            client.on_event(move |event| {
                if let SessionEvent::Subscribed { topic, .. } = event
                    && topic == &topic_ev
                {
                    let hello = driver.borrow().hello();
                    if let Some(client) = weak.upgrade() {
                        client.send_data(&topic_ev, COLLAB_SUBPROTOCOL, hello.encode());
                    }
                }
            });
        }

        // Inbound collab messages drive the handshake.
        {
            let weak: Weak<RealtimeClient> = Rc::downgrade(&client);
            let driver = driver.clone();
            let topic_cb = topic.clone();
            let on_change = on_change.clone();
            client.subscribe(&topic, COLLAB_SUBPROTOCOL, move |payload| {
                // Decode + advance the driver, then release its borrow BEFORE
                // sending replies / notifying (a handler may re-enter).
                let outcome = match driver.borrow_mut().on_payload(&payload) {
                    Ok(outcome) => outcome,
                    Err(err) => {
                        // A malformed or un-appliable message: surface it rather
                        // than wedging convergence silently.
                        tracing::warn!(target: "pocopine.log", error = %err, "collab message dropped");
                        return;
                    }
                };
                if let Some(reply) = &outcome.reply
                    && let Some(client) = weak.upgrade()
                {
                    client.send_data(&topic_cb, COLLAB_SUBPROTOCOL, reply.encode());
                }
                if let Some(node) = &outcome.document {
                    // Clone the handler Rc out and drop the borrow before calling
                    // it, so the callback can re-enter the connection safely.
                    let handler = on_change.borrow().clone();
                    if let Some(handler) = handler {
                        handler(node);
                    }
                }
            });
        }

        Ok(Self {
            client,
            driver,
            topic,
            on_change,
        })
    }

    /// Register a callback fired with the new document whenever a remote change
    /// is merged — wire it to load the document into the view editor. A no-op
    /// apply (the gateway echoing your own edit, or a duplicate) does NOT fire it.
    ///
    /// A connection drives exactly ONE editor, so registering a second handler
    /// (e.g. a second [`bind`](Self::bind)) is a [`BindError::AlreadyBound`]
    /// error rather than a silent detach of the first — open a separate
    /// [`CollabConnection`] per editor.
    pub fn on_change(&self, callback: impl Fn(&Node) + 'static) -> Result<(), BindError> {
        let mut slot = self.on_change.borrow_mut();
        if slot.is_some() {
            return Err(BindError::AlreadyBound);
        }
        *slot = Some(Rc::new(callback));
        Ok(())
    }

    /// Commit a local edit coarsely (a whole-document rebuild) and broadcast it.
    /// Prefer [`push_local_steps`](Self::push_local_steps) when the editor's
    /// transaction steps are available — it converges concurrent in-block edits
    /// and keeps peers' cursors alive.
    pub fn push_local(&self, doc: &Node) -> Result<(), BindError> {
        self.push_local_steps(doc, &[])
    }

    /// Commit a local edit as a fine-grained incremental update from the editor's
    /// transaction `steps` (`new_doc` is the document after them) and broadcast it.
    /// Structural steps fall back to a coarse rebuild. The broadcast is queued by
    /// the transport until the socket is OPEN and the topic is bound.
    pub fn push_local_steps(
        &self,
        new_doc: &Node,
        steps: &[pine_richtext::transform::Step],
    ) -> Result<(), BindError> {
        let update = self.driver.borrow_mut().push_local_steps(new_doc, steps)?;
        self.client
            .send_data(&self.topic, COLLAB_SUBPROTOCOL, update.encode());
        Ok(())
    }

    /// Anchor a model caret/selection endpoint to a [`StickyPoint`](crate::StickyPoint)
    /// before applying a remote change, then [`resolve_point`](Self::resolve_point)
    /// it after to restore the caret where it moved to.
    pub fn point_at(&self, model_pos: usize) -> Result<Option<crate::StickyPoint>, BindError> {
        self.driver
            .borrow()
            .point_at(model_pos)
            .map_err(BindError::from)
    }

    /// Resolve a [`StickyPoint`](crate::StickyPoint) to its current model position.
    pub fn resolve_point(&self, point: &crate::StickyPoint) -> Result<Option<usize>, BindError> {
        self.driver
            .borrow()
            .resolve_point(point)
            .map_err(BindError::from)
    }

    /// The current document.
    pub fn document(&self) -> Result<Node, BindError> {
        self.driver.borrow().document().map_err(BindError::from)
    }

    /// Wire a mounted rich-text [`Editor`](pine_richtext::view::Editor) to this
    /// connection end to end: local edits become *fine-grained* incremental CRDT
    /// writes (so concurrent in-block edits converge), and remote edits load the
    /// converged document back into the editor. Returns the editor-update
    /// subscription — **keep it alive** for the session (dropping it unsubscribes).
    ///
    /// Loop-safe: the remote load goes through `ReplaceState`, which does not
    /// re-fire the editor's update event, so it never echoes back as a local edit;
    /// and the driver suppresses a no-op apply of our own echoed update.
    ///
    /// Caret preservation across a remote edit (A2) needs editor selection get/set
    /// APIs that don't exist yet, so a remote edit currently reloads the doc; the
    /// [`point_at`](Self::point_at) / [`resolve_point`](Self::resolve_point)
    /// primitives are exposed for callers wiring it by hand in the meantime.
    ///
    /// Errors with [`BindError::AlreadyBound`] if this connection already drives
    /// an editor (one connection ↔ one editor).
    #[cfg(target_arch = "wasm32")]
    pub fn bind(
        self: &Rc<Self>,
        editor: &pine_richtext::view::Editor,
    ) -> Result<pine_richtext::view::DocChangeSubscription, BindError> {
        use pine_richtext::view::DocNode;

        // Remote edits → load the converged doc back into the editor. Registered
        // FIRST so a double-bind errors out *before* we attach the editor's
        // update subscription (nothing to unwind on the error path).
        let editor_remote = editor.clone();
        self.on_change(move |node| {
            let _ = editor_remote.set::<DocNode>(node);
        })?;

        // Local edits → a fine-grained incremental write.
        let weak = Rc::downgrade(self);
        let sub = editor.on_update_steps::<DocNode, _>(move |node, steps| {
            if let Some(conn) = weak.upgrade()
                && let Err(err) = conn.push_local_steps(&node, &steps)
            {
                tracing::warn!(target: "pocopine.log", error = %err, "collab: local push failed");
            }
        });
        Ok(sub)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::schema_basic;
    use wasm_bindgen_test::wasm_bindgen_test;

    fn para(text: &str) -> Node {
        schema_basic::paragraph(vec![schema_basic::text(text, vec![]).unwrap()]).unwrap()
    }

    // Smoke test of the web_sys glue (the protocol logic is host-tested via the
    // driver). The socket never reaches OPEN against an unreachable URL, so this
    // exercises the readiness queue + Drop path: open must not throw, push_local
    // must queue (not drop) and return Ok, on_change must register, and Drop must
    // clear handlers without panicking. Run with `wasm-pack test --node`.
    #[wasm_bindgen_test]
    fn open_queues_without_throwing_and_drops_cleanly() {
        let conn = CollabConnection::open(
            "ws://127.0.0.1:1/__pocopine/ws/v1",
            "collab:smoke",
            1,
            schema_basic::schema(),
        )
        .expect("open constructs while the socket is CONNECTING");

        // A pre-OPEN edit is queued (not dropped) and reported Ok.
        conn.push_local(&schema_basic::doc(vec![para("hi")]).unwrap())
            .expect("push_local queues without error");

        // The first change handler registers; a second is refused (one editor
        // per connection).
        conn.on_change(|_| {}).expect("first on_change registers");
        assert!(
            matches!(conn.on_change(|_| {}), Err(BindError::AlreadyBound)),
            "a second on_change must be AlreadyBound, not a silent replace"
        );

        // Drop clears the (absent in node) heartbeat + detaches handlers.
        drop(conn);
    }
}
