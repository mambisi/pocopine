//! The `wasm32` WebSocket I/O shell driving [`ClientSession`].
//!
//! Browser-only (`web_sys::WebSocket`). The protocol logic lives in
//! [`ClientSession`] (host-tested); this is the thin glue: decode inbound
//! frames → feed the session → route Data payloads to per-topic handlers, and
//! encode outbound frames the session produces.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bytes::Bytes;
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, MessageEvent, WebSocket};

use super::session::{ClientSession, SessionEvent};
use crate::protocol::{Frame, WS_PROTOCOL_V1};

/// A per-topic Data handler. `Rc` so the message callback can clone it out and
/// invoke it *after* releasing the borrow (the handler may re-enter the client,
/// e.g. to send a sync reply).
type DataHandler = Rc<dyn Fn(Bytes)>;
/// Optional observer of every session event (connected, subscribed, gap, …).
type EventHandler = Rc<dyn Fn(&SessionEvent)>;

struct Inner {
    ws: WebSocket,
    session: ClientSession,
    on_data: HashMap<String, DataHandler>,
    on_event: Option<EventHandler>,
}

/// A live connection to a `pocopine-realtime` gateway from the browser.
pub struct RealtimeClient {
    inner: Rc<RefCell<Inner>>,
    // The message callback owns an `Rc` clone of `inner`; keep it alive for the
    // client's lifetime (dropping it detaches the handler).
    _on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl RealtimeClient {
    /// Open a connection to `url`, advertising the `pocopine.ws.v1` sub-protocol.
    pub fn connect(url: &str) -> Result<Self, JsValue> {
        let ws = WebSocket::new_with_str(url, WS_PROTOCOL_V1)?;
        ws.set_binary_type(BinaryType::Arraybuffer);

        let inner = Rc::new(RefCell::new(Inner {
            ws: ws.clone(),
            session: ClientSession::new(),
            on_data: HashMap::new(),
            on_event: None,
        }));

        let inner_cb = inner.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() else {
                return; // not a binary frame; ignore
            };
            let bytes = Uint8Array::new(&buffer).to_vec();
            let Ok(frame) = Frame::decode(&bytes) else {
                return; // malformed frame; the session would just ignore it
            };

            // Advance the session and pull out anything to invoke, WITHOUT
            // holding the borrow across a handler call (handlers may re-enter).
            let (event, data_handler, observer) = {
                let mut inner = inner_cb.borrow_mut();
                let event = inner.session.on_frame(&frame).ok().flatten();
                let data_handler = match &event {
                    Some(SessionEvent::Data { topic, .. }) => inner.on_data.get(topic).cloned(),
                    _ => None,
                };
                let observer = inner.on_event.clone();
                (event, data_handler, observer)
            };

            if let (Some(SessionEvent::Data { payload, .. }), Some(handler)) =
                (&event, &data_handler)
            {
                handler(payload.clone());
            }
            if let (Some(observer), Some(event)) = (observer, &event) {
                observer(event);
            }
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        Ok(Self {
            inner,
            _on_message: on_message,
        })
    }

    /// Observe every session lifecycle event (Connected, Subscribed, Gap, …).
    pub fn on_event(&self, handler: impl Fn(&SessionEvent) + 'static) {
        self.inner.borrow_mut().on_event = Some(Rc::new(handler));
    }

    /// Subscribe to `topic` under `subprotocol_id`, routing its Data payloads to
    /// `on_data`.
    pub fn subscribe(&self, topic: &str, subprotocol_id: u64, on_data: impl Fn(Bytes) + 'static) {
        let mut inner = self.inner.borrow_mut();
        inner.on_data.insert(topic.to_string(), Rc::new(on_data));
        let frame = inner.session.subscribe(topic, subprotocol_id);
        let _ = inner.ws.send_with_u8_array(&frame.encode());
    }

    /// Send `payload` to `topic` (a no-op until the subscribe is acknowledged
    /// and a `topic_ref` is bound).
    pub fn send_data(&self, topic: &str, subprotocol_id: u64, payload: Bytes) {
        let inner = self.inner.borrow();
        if let Some(frame) = inner.session.data(topic, subprotocol_id, payload) {
            let _ = inner.ws.send_with_u8_array(&frame.encode());
        }
    }

    /// Send a liveness heartbeat. The app drives this on a timer at
    /// [`ClientSession::heartbeat_interval_ms`]; auto-wiring `setInterval` is a
    /// follow-up.
    pub fn heartbeat(&self) {
        let inner = self.inner.borrow();
        if let Ok(frame) = inner.session.heartbeat() {
            let _ = inner.ws.send_with_u8_array(&frame.encode());
        }
    }

    /// The interval (ms) the server asked the client to heartbeat at (0 until
    /// the `Hello` has arrived).
    pub fn heartbeat_interval_ms(&self) -> u32 {
        self.inner.borrow().session.heartbeat_interval_ms()
    }
}
