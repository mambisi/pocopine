//! The `wasm32` WebSocket I/O shell driving [`ClientSession`].
//!
//! Browser-only (`web_sys::WebSocket`). The protocol logic lives in
//! [`ClientSession`] (host-tested); this is the thin glue: decode inbound
//! frames → feed the session → route Data payloads to per-topic handlers, and
//! encode outbound frames the session produces.
//!
//! ## Readiness queue
//!
//! A `WebSocket` starts in `CONNECTING`; `send()` throws until it reaches
//! `OPEN`, and a topic can't be addressed until its `SubscribeAck` binds a
//! `topic_ref`. So every outbound frame is queued and [`Inner::flush`]ed when it
//! becomes sendable — on `onopen`, and again whenever a `SubscribeAck` binds a
//! ref. Callers therefore never race the handshake.
//!
//! ## Liveness
//!
//! Once the server's `Hello` arrives, a `setInterval` heartbeat is started at
//! the advertised interval so the connection isn't reaped as a zombie. The timer
//! is cleared on drop.

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

/// An outbound frame not yet sendable.
enum Outbound {
    /// A pre-encoded frame that only needs the socket OPEN (subscribe, heartbeat).
    Frame(Vec<u8>),
    /// Topic data that needs OPEN *and* a bound `topic_ref` to be addressed.
    Data {
        topic: String,
        subprotocol_id: u64,
        payload: Bytes,
    },
}

struct Inner {
    ws: WebSocket,
    session: ClientSession,
    on_data: HashMap<String, DataHandler>,
    on_event: Option<EventHandler>,
    /// True once `onopen` has fired; sends are queued until then.
    open: bool,
    /// Outbound frames waiting for OPEN (and, for Data, a bound ref).
    outbox: Vec<Outbound>,
    /// The heartbeat timer: its closure (kept alive) and the interval handle.
    heartbeat: Option<(Closure<dyn FnMut()>, i32)>,
}

impl Inner {
    /// Send everything queued that is now sendable. Data whose topic_ref is not
    /// yet bound is left queued for the next flush (after its SubscribeAck).
    fn flush(&mut self) {
        if !self.open {
            return;
        }
        for item in std::mem::take(&mut self.outbox) {
            match item {
                Outbound::Frame(bytes) => {
                    let _ = self.ws.send_with_u8_array(&bytes);
                }
                Outbound::Data {
                    topic,
                    subprotocol_id,
                    payload,
                } => match self.session.data(&topic, subprotocol_id, payload.clone()) {
                    Some(frame) => {
                        let _ = self.ws.send_with_u8_array(&frame.encode());
                    }
                    None => self.outbox.push(Outbound::Data {
                        topic,
                        subprotocol_id,
                        payload,
                    }),
                },
            }
        }
    }
}

/// A live connection to a `pocopine-realtime` gateway from the browser.
pub struct RealtimeClient {
    inner: Rc<RefCell<Inner>>,
    // The socket callbacks own `Rc` clones of `inner`; keep them alive for the
    // client's lifetime (dropping them detaches the handlers).
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_open: Closure<dyn FnMut()>,
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
            open: false,
            outbox: Vec::new(),
            heartbeat: None,
        }));

        // onopen: the socket can send now — drain the queue.
        let on_open = {
            let inner = inner.clone();
            Closure::<dyn FnMut()>::new(move || {
                let mut guard = inner.borrow_mut();
                guard.open = true;
                guard.flush();
            })
        };
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let inner_cb = inner.clone();
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() else {
                return; // not a binary frame; ignore
            };
            let bytes = Uint8Array::new(&buffer).to_vec();
            let Ok(frame) = Frame::decode(&bytes) else {
                tracing::warn!(target: "pocopine.log", "realtime: undecodable frame dropped");
                return;
            };

            // Advance the session and pull out anything to invoke, WITHOUT
            // holding the borrow across a handler call (handlers may re-enter).
            let (event, data_handler, observer, start_heartbeat) = {
                let mut guard = inner_cb.borrow_mut();
                let event = match guard.session.on_frame(&frame) {
                    Ok(event) => event,
                    Err(err) => {
                        tracing::warn!(target: "pocopine.log", error = %err, "realtime: bad control frame dropped");
                        None
                    }
                };
                // A SubscribeAck may have bound a topic_ref — flush queued data.
                if matches!(event, Some(SessionEvent::Subscribed { .. })) {
                    guard.flush();
                }
                let data_handler = match &event {
                    Some(SessionEvent::Data { topic, .. }) => guard.on_data.get(topic).cloned(),
                    _ => None,
                };
                let observer = guard.on_event.clone();
                let start_heartbeat = matches!(event, Some(SessionEvent::Connected { .. }))
                    && guard.heartbeat.is_none();
                (event, data_handler, observer, start_heartbeat)
            };

            if start_heartbeat {
                Self::start_heartbeat(&inner_cb);
            }
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
            _on_open: on_open,
        })
    }

    /// Start the `setInterval` heartbeat at the server-advertised interval. Idle
    /// connections are otherwise reaped as zombies after a few missed beats.
    fn start_heartbeat(inner: &Rc<RefCell<Inner>>) {
        let interval_ms = inner.borrow().session.heartbeat_interval_ms();
        let Some(window) = web_sys::window() else {
            return;
        };
        if interval_ms == 0 {
            return;
        }

        let tick_inner = inner.clone();
        let tick = Closure::<dyn FnMut()>::new(move || {
            let guard = tick_inner.borrow();
            if guard.open
                && let Ok(frame) = guard.session.heartbeat()
            {
                let _ = guard.ws.send_with_u8_array(&frame.encode());
            }
        });

        if let Ok(handle) = window.set_interval_with_callback_and_timeout_and_arguments_0(
            tick.as_ref().unchecked_ref(),
            interval_ms as i32,
        ) {
            inner.borrow_mut().heartbeat = Some((tick, handle));
        }
    }

    /// Observe every session lifecycle event (Connected, Subscribed, Gap, …).
    pub fn on_event(&self, handler: impl Fn(&SessionEvent) + 'static) {
        self.inner.borrow_mut().on_event = Some(Rc::new(handler));
    }

    /// Subscribe to `topic` under `subprotocol_id`, routing its Data payloads to
    /// `on_data`. The Subscribe frame is queued until the socket is OPEN.
    pub fn subscribe(&self, topic: &str, subprotocol_id: u64, on_data: impl Fn(Bytes) + 'static) {
        let mut guard = self.inner.borrow_mut();
        guard.on_data.insert(topic.to_string(), Rc::new(on_data));
        let frame = guard.session.subscribe(topic, subprotocol_id);
        guard.outbox.push(Outbound::Frame(frame.encode()));
        guard.flush();
    }

    /// Queue `payload` for `topic`; it is sent once the socket is OPEN and the
    /// topic's `SubscribeAck` has bound a ref. Never silently dropped.
    pub fn send_data(&self, topic: &str, subprotocol_id: u64, payload: Bytes) {
        let mut guard = self.inner.borrow_mut();
        guard.outbox.push(Outbound::Data {
            topic: topic.to_string(),
            subprotocol_id,
            payload,
        });
        guard.flush();
    }

    /// The interval (ms) the server asked the client to heartbeat at (0 until
    /// the `Hello` has arrived). The heartbeat is driven automatically.
    pub fn heartbeat_interval_ms(&self) -> u32 {
        self.inner.borrow().session.heartbeat_interval_ms()
    }
}

impl Drop for RealtimeClient {
    fn drop(&mut self) {
        // Clear the heartbeat interval, detach the socket handlers, and close.
        let heartbeat = self.inner.borrow_mut().heartbeat.take();
        if let Some((_closure, handle)) = heartbeat
            && let Some(window) = web_sys::window()
        {
            window.clear_interval_with_handle(handle);
        }
        let guard = self.inner.borrow();
        guard.ws.set_onopen(None);
        guard.ws.set_onmessage(None);
        let _ = guard.ws.close();
    }
}
