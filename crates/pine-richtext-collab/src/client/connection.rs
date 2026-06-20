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

/// A live collaborative rich-text session over the realtime gateway.
///
/// # Warning — v1 is single-writer
///
/// [`push_local`](Self::push_local) re-encodes the whole document (the coarse v1
/// write). Two clients editing the same doc concurrently **silently lose** each
/// other's edits — the re-encode tombstones the shared subtree. v1 is safe only
/// with one writer at a time; nothing here enforces that yet, so a multi-writer
/// deployment needs an external lease until the Phase-5 incremental write lands.
pub struct CollabConnection {
    client: Rc<RealtimeClient>,
    driver: Rc<RefCell<CollabSyncClient>>,
    topic: String,
    on_change: ChangeHandler,
}

impl CollabConnection {
    /// Open a collaborative session: connect to `url`, subscribe to `topic`
    /// under the collab sub-protocol, and run the sync handshake. `client_id`
    /// must be globally unique (e.g. the realtime `ws-N` session number).
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
    pub fn on_change(&self, callback: impl Fn(&Node) + 'static) {
        *self.on_change.borrow_mut() = Some(Rc::new(callback));
    }

    /// Commit a local edit (the coarse v1 write) and broadcast it to peers.
    /// Returns `Err` if the document can't be encoded; the broadcast itself is
    /// queued by the transport until the socket is OPEN and the topic is bound.
    pub fn push_local(&self, doc: &Node) -> Result<(), BindError> {
        let update = self.driver.borrow_mut().push_local(doc)?;
        self.client
            .send_data(&self.topic, COLLAB_SUBPROTOCOL, update.encode());
        Ok(())
    }

    /// The current document.
    pub fn document(&self) -> Result<Node, BindError> {
        self.driver.borrow().document().map_err(BindError::from)
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

        // Registering a change handler must not panic.
        conn.on_change(|_| {});

        // Drop clears the (absent in node) heartbeat + detaches handlers.
        drop(conn);
    }
}
