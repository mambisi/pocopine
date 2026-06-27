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
//!
//! ## Reconnection
//!
//! The socket dies for reasons outside our control — the server reaps an idle
//! connection (a backgrounded tab's `setInterval` is throttled below the
//! heartbeat rate), a proxy times out, the network blips. `onclose` then stops
//! the heartbeat and schedules a reconnect with capped exponential backoff
//! ([`reconnect_delay_ms`]). On reconnect the transport opens a fresh socket and
//! re-subscribes every joined topic, so a consumer that re-syncs on
//! [`SessionEvent::Subscribed`] (the collab handshake does) recovers
//! automatically. Sends are guarded by [`Inner::is_sendable`] so a frame is
//! never pushed at a `CLOSING`/`CLOSED` socket (which throws).
//!
//! All socket-event closures and timer callbacks capture a `Weak<RefCell<Inner>>`
//! (never a strong `Rc`), so storing them on `Inner` introduces no reference
//! cycle: [`RealtimeClient`] is the sole strong owner and its `Drop` tears the
//! whole graph down.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use bytes::Bytes;
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use super::session::{ClientSession, ConnectionStatus, SessionEvent, reconnect_delay_ms};
use crate::protocol::{Frame, WS_PROTOCOL_V1};

/// A per-topic Data handler. `Rc` so the message callback can clone it out and
/// invoke it *after* releasing the borrow (the handler may re-enter the client,
/// e.g. to send a sync reply).
type DataHandler = Rc<dyn Fn(Bytes)>;
/// Optional observer of every session event (connected, subscribed, gap, …).
type EventHandler = Rc<dyn Fn(&SessionEvent)>;
/// Optional observer of the connection's transport-level liveness.
type StatusHandler = Rc<dyn Fn(ConnectionStatus)>;

/// Cap on queued `Data` frames during an outage; past this the oldest is dropped
/// to bound memory. Safe for CRDT topics — the reconnect SyncStep1/2 handshake
/// re-reconciles full state — so a dropped delta is recovered. Relay topics that
/// need durable offline delivery must bound at the app layer.
const MAX_QUEUED_DATA: usize = 1024;

/// WebSocket close code for a policy violation (RFC 6455 §7.4.1) — treated as a
/// fatal, non-retryable close (auth / forbidden topic): the client stops
/// reconnecting and transitions to `Closed` rather than looping forever.
const CLOSE_CODE_POLICY_VIOLATION: u16 = 1008;

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

/// The reusable socket-event closures, re-attached to each new socket across
/// reconnects. They capture a `Weak<RefCell<Inner>>`, so holding them here does
/// not pin `Inner` alive.
struct SocketHandlers {
    on_open: Closure<dyn FnMut()>,
    on_message: Closure<dyn FnMut(MessageEvent)>,
    on_close: Closure<dyn FnMut(CloseEvent)>,
    on_error: Closure<dyn FnMut(Event)>,
}

struct Inner {
    ws: WebSocket,
    /// The gateway URL, kept so a dropped socket can be re-opened.
    url: String,
    session: ClientSession,
    on_data: HashMap<String, DataHandler>,
    /// Subscription intent (topic → subprotocol_id), replayed on every reconnect.
    subscriptions: HashMap<String, u64>,
    on_event: Option<EventHandler>,
    on_status: Option<StatusHandler>,
    /// True once `onopen` has fired; sends are queued until then.
    open: bool,
    /// Outbound frames waiting for OPEN (and, for Data, a bound ref).
    outbox: Vec<Outbound>,
    /// The heartbeat timer: its closure (kept alive) and the interval handle.
    heartbeat: Option<(Closure<dyn FnMut()>, i32)>,
    /// A pending reconnect timer: its closure (kept alive) and the timeout handle.
    reconnect: Option<(Closure<dyn FnMut()>, i32)>,
    /// Consecutive dropped connections, for backoff. Reset to 0 once the
    /// session's `Hello` arrives (a true session, not just an open socket).
    reconnect_attempts: u32,
    /// The socket-event closures, re-attached to each new socket on reconnect.
    handlers: Option<SocketHandlers>,
    /// Set on `Drop`/`close()` so a late close event or timer cannot resurrect
    /// the socket.
    closed: bool,
    /// True once an outbox overflow has been logged this outage, so a long
    /// disconnected drag doesn't spam the log on every dropped frame.
    outbox_overflow_warned: bool,
}

impl Inner {
    /// True when a frame may be written now: the session is open *and* the live
    /// socket is actually `OPEN` (guards the `CLOSING`/`CLOSED` race, which would
    /// otherwise throw "WebSocket is already in CLOSING or CLOSED state").
    fn is_sendable(&self) -> bool {
        self.open && self.ws.ready_state() == WebSocket::OPEN
    }

    /// Send everything queued that is now sendable. Data whose topic_ref is not
    /// yet bound is left queued for the next flush (after its SubscribeAck).
    fn flush(&mut self) {
        if !self.is_sendable() {
            return;
        }
        // The queue is draining — re-arm the one-shot overflow warning for the
        // next outage.
        self.outbox_overflow_warned = false;
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

    /// Queue a Data payload, bounding the backlog at [`MAX_QUEUED_DATA`] so a long
    /// outage can't grow it without limit. Past the cap the oldest queued Data is
    /// dropped (Subscribe frames are kept — they must replay); a CRDT topic still
    /// recovers via the reconnect SyncStep1/2 handshake.
    fn queue_data(&mut self, topic: String, subprotocol_id: u64, payload: Bytes) {
        self.outbox.push(Outbound::Data {
            topic,
            subprotocol_id,
            payload,
        });
        if self.outbox.len() > MAX_QUEUED_DATA
            && let Some(oldest) = self
                .outbox
                .iter()
                .position(|item| matches!(item, Outbound::Data { .. }))
        {
            self.outbox.remove(oldest);
            if !self.outbox_overflow_warned {
                self.outbox_overflow_warned = true;
                tracing::warn!(
                    target: "pocopine.log",
                    cap = MAX_QUEUED_DATA,
                    "realtime: outbox overflow during outage; dropping oldest queued data (state recovers on reconnect resync)"
                );
            }
        }
    }

    /// Clear the heartbeat interval (and drop its closure) if running.
    fn stop_heartbeat(&mut self) {
        if let Some((_closure, handle)) = self.heartbeat.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_interval_with_handle(handle);
        }
    }

    /// Permanently tear the connection down: mark closed (so a late close event or
    /// timer is a no-op), clear both timers, detach the socket handlers, and close
    /// the socket. Idempotent. Shared by `Drop` and [`RealtimeClient::close`].
    fn teardown(&mut self) {
        self.closed = true;
        self.open = false;
        self.stop_heartbeat();
        if let Some((_closure, handle)) = self.reconnect.take()
            && let Some(window) = web_sys::window()
        {
            window.clear_timeout_with_handle(handle);
        }
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        self.ws.set_onerror(None);
        let _ = self.ws.close();
    }
}

/// A live connection to a `pocopine-realtime` gateway from the browser.
///
/// The socket reconnects automatically when it drops (see the module docs),
/// re-subscribing every joined topic; observe [`RealtimeClient::on_status`] to
/// reflect connection state in a UI.
pub struct RealtimeClient {
    // The sole strong owner of `Inner`; every closure holds only a `Weak`, so
    // dropping this tears the whole graph down (see `Drop`).
    inner: Rc<RefCell<Inner>>,
}

impl RealtimeClient {
    /// Open a connection to `url`, advertising the `pocopine.ws.v1` sub-protocol.
    pub fn connect(url: &str) -> Result<Self, JsValue> {
        let ws = WebSocket::new_with_str(url, WS_PROTOCOL_V1)?;
        ws.set_binary_type(BinaryType::Arraybuffer);

        let inner = Rc::new(RefCell::new(Inner {
            ws: ws.clone(),
            url: url.to_string(),
            session: ClientSession::new(),
            on_data: HashMap::new(),
            subscriptions: HashMap::new(),
            on_event: None,
            on_status: None,
            open: false,
            outbox: Vec::new(),
            heartbeat: None,
            reconnect: None,
            reconnect_attempts: 0,
            handlers: None,
            closed: false,
            outbox_overflow_warned: false,
        }));

        let handlers = Self::build_handlers(&inner);
        Self::attach(&ws, &handlers);
        inner.borrow_mut().handlers = Some(handlers);

        Ok(Self { inner })
    }

    /// Open a connection with a browser-safe bearer token query parameter.
    ///
    /// Browsers cannot set arbitrary WebSocket upgrade headers. This appends an
    /// `access_token` query parameter and expects the server to mount
    /// `routes_with_auth` with a compatible `AuthProvider`.
    pub fn connect_with_token(url: &str, token: &str) -> Result<Self, JsValue> {
        Self::connect(&super::url_with_access_token(url, token))
    }

    /// Build the four reusable socket-event closures (each holding a `Weak`).
    fn build_handlers(inner: &Rc<RefCell<Inner>>) -> SocketHandlers {
        let on_open = {
            let weak = Rc::downgrade(inner);
            Closure::<dyn FnMut()>::new(move || {
                if let Some(inner) = weak.upgrade() {
                    Self::handle_open(&inner);
                }
            })
        };
        let on_message = {
            let weak = Rc::downgrade(inner);
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                if let Some(inner) = weak.upgrade() {
                    Self::handle_message(&inner, event);
                }
            })
        };
        let on_close = {
            let weak = Rc::downgrade(inner);
            Closure::<dyn FnMut(CloseEvent)>::new(move |event: CloseEvent| {
                if let Some(inner) = weak.upgrade() {
                    Self::handle_close(&inner, event.code());
                }
            })
        };
        // A WebSocket error is always followed by a close event, which drives the
        // reconnect; here we only log so the failure isn't silent.
        let on_error = Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
            tracing::warn!(target: "pocopine.log", "realtime: socket error (reconnect follows on close)");
        });

        SocketHandlers {
            on_open,
            on_message,
            on_close,
            on_error,
        }
    }

    /// Bind the handlers to a (fresh) socket.
    fn attach(ws: &WebSocket, handlers: &SocketHandlers) {
        ws.set_onopen(Some(handlers.on_open.as_ref().unchecked_ref()));
        ws.set_onmessage(Some(handlers.on_message.as_ref().unchecked_ref()));
        ws.set_onclose(Some(handlers.on_close.as_ref().unchecked_ref()));
        ws.set_onerror(Some(handlers.on_error.as_ref().unchecked_ref()));
    }

    /// `onopen`: the socket can send now — drain the queue. The backoff is reset
    /// later, on the `Hello` (see [`Self::handle_message`]): a socket that opens
    /// but never completes the session handshake must keep backing off, not reset
    /// here and thrash at the base delay.
    fn handle_open(inner: &Rc<RefCell<Inner>>) {
        {
            let mut guard = inner.borrow_mut();
            guard.open = true;
            guard.flush();
        }
        Self::emit_status(inner, ConnectionStatus::Open);
    }

    /// One inbound message: decode → advance the session → route, without holding
    /// the borrow across a handler call (handlers may re-enter the client).
    fn handle_message(inner: &Rc<RefCell<Inner>>, event: MessageEvent) {
        let Ok(buffer) = event.data().dyn_into::<ArrayBuffer>() else {
            return; // not a binary frame; ignore
        };
        let bytes = Uint8Array::new(&buffer).to_vec();
        let Ok(frame) = Frame::decode(&bytes) else {
            tracing::warn!(target: "pocopine.log", "realtime: undecodable frame dropped");
            return;
        };

        let (event, data_handler, observer, start_heartbeat) = {
            let mut guard = inner.borrow_mut();
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
            // A fresh `Hello` means the session is truly established — only now is
            // it safe to reset the reconnect backoff. Resetting on `onopen` would
            // let an open-then-immediately-close loop thrash at the base delay.
            if matches!(event, Some(SessionEvent::Connected { .. })) {
                guard.reconnect_attempts = 0;
            }
            let start_heartbeat =
                matches!(event, Some(SessionEvent::Connected { .. })) && guard.heartbeat.is_none();
            (event, data_handler, observer, start_heartbeat)
        };

        if start_heartbeat {
            Self::start_heartbeat(inner);
        }
        if let (Some(SessionEvent::Data { payload, .. }), Some(handler)) = (&event, &data_handler) {
            handler(payload.clone());
        }
        if let (Some(observer), Some(event)) = (observer, &event) {
            observer(event);
        }
    }

    /// `onclose`: stop the heartbeat (so it can't keep firing at a dead socket)
    /// and either reconnect (transient close) or give up (fatal close). Idempotent
    /// and a no-op once dropped.
    fn handle_close(inner: &Rc<RefCell<Inner>>, code: u16) {
        // A policy-violation close (auth / forbidden topic) is not retryable —
        // reconnecting would just be rejected again. Tear down and go Closed.
        let fatal = code == CLOSE_CODE_POLICY_VIOLATION;
        {
            let mut guard = inner.borrow_mut();
            if guard.closed {
                return;
            }
            guard.open = false;
            guard.stop_heartbeat();
            if fatal {
                guard.teardown();
            }
        }
        if fatal {
            tracing::warn!(target: "pocopine.log", code, "realtime: fatal close; not reconnecting");
            Self::emit_status(inner, ConnectionStatus::Closed);
            return;
        }
        Self::emit_status(inner, ConnectionStatus::Reconnecting);
        Self::schedule_reconnect(inner);
    }

    /// Arm a `setTimeout` to reconnect after the backoff for the current attempt
    /// count, replacing any pending timer. Reconnects retry indefinitely (capped
    /// cadence) so a server restart is eventually recovered.
    fn schedule_reconnect(inner: &Rc<RefCell<Inner>>) {
        let Some(window) = web_sys::window() else {
            return;
        };
        let base = {
            let mut guard = inner.borrow_mut();
            if guard.closed {
                return;
            }
            // Cancel any still-pending timer so at most one reconnect is ever
            // armed. Without the `clear_timeout`, a second close (or the reconnect
            // error path) would leave the old `setTimeout` live; when it later
            // fired it would invoke an already-dropped closure.
            if let Some((_closure, handle)) = guard.reconnect.take() {
                window.clear_timeout_with_handle(handle);
            }
            let base = reconnect_delay_ms(guard.reconnect_attempts);
            guard.reconnect_attempts = guard.reconnect_attempts.saturating_add(1);
            base
        };
        // ±20% jitter so a fleet of clients reconnecting after a server blip don't
        // synchronize into a thundering herd. The pure `reconnect_delay_ms` stays
        // deterministic (host-tested); randomness lives only here, on wasm.
        let factor = 0.8 + 0.4 * js_sys::Math::random();
        let delay = (f64::from(base) * factor) as i32;

        let weak = Rc::downgrade(inner);
        let timer = Closure::<dyn FnMut()>::new(move || {
            if let Some(inner) = weak.upgrade() {
                Self::reconnect(&inner);
            }
        });
        if let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            timer.as_ref().unchecked_ref(),
            delay,
        ) {
            inner.borrow_mut().reconnect = Some((timer, handle));
        }
    }

    /// Open a fresh socket, re-attach the handlers, reset the session, and
    /// re-queue a Subscribe for every joined topic so the consumer's
    /// `Subscribed` handshake re-runs and recovers state.
    fn reconnect(inner: &Rc<RefCell<Inner>>) {
        let url = {
            let guard = inner.borrow();
            if guard.closed {
                return;
            }
            guard.url.clone()
        };

        let ws = match WebSocket::new_with_str(&url, WS_PROTOCOL_V1) {
            Ok(ws) => ws,
            Err(err) => {
                tracing::warn!(target: "pocopine.log", error = ?err, "realtime: reconnect failed to open; backing off");
                Self::schedule_reconnect(inner);
                return;
            }
        };
        ws.set_binary_type(BinaryType::Arraybuffer);

        let mut guard = inner.borrow_mut();
        // Detach the OLD socket's handlers before swapping it out. The dead socket
        // stays alive in the browser's event system; a delayed event from it (a
        // late `onclose`, or an in-flight `onmessage`) must not reach `handle_*`
        // and corrupt the NEW socket's state or mis-route a stale frame.
        guard.ws.set_onopen(None);
        guard.ws.set_onmessage(None);
        guard.ws.set_onclose(None);
        guard.ws.set_onerror(None);
        if let Some(handlers) = &guard.handlers {
            Self::attach(&ws, handlers);
        }
        guard.ws = ws;
        guard.open = false;
        // A reconnect is a brand-new server session: drop the stale topic_refs so
        // an old ref can't alias a new one. on_data routes, subscription intents,
        // observers, and handlers all live on `Inner` and survive.
        guard.session = ClientSession::new();

        // Rebuild the outbox: a fresh Subscribe for every joined topic, ahead of
        // any data queued during the outage (which waits for the new SubscribeAck
        // to bind a ref, exactly as on first connect).
        let pending: Vec<Outbound> = guard
            .outbox
            .drain(..)
            .filter(|item| matches!(item, Outbound::Data { .. }))
            .collect();
        let subs: Vec<(String, u64)> = guard
            .subscriptions
            .iter()
            .map(|(topic, subprotocol_id)| (topic.clone(), *subprotocol_id))
            .collect();
        let mut outbox = Vec::with_capacity(subs.len() + pending.len());
        for (topic, subprotocol_id) in subs {
            let frame = guard.session.subscribe(&topic, subprotocol_id);
            outbox.push(Outbound::Frame(frame.encode()));
        }
        outbox.extend(pending);
        guard.outbox = outbox;
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

        let weak = Rc::downgrade(inner);
        let tick = Closure::<dyn FnMut()>::new(move || {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let guard = inner.borrow();
            if guard.is_sendable()
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

    /// Invoke the status observer (if any) after releasing the borrow.
    fn emit_status(inner: &Rc<RefCell<Inner>>, status: ConnectionStatus) {
        let handler = inner.borrow().on_status.clone();
        if let Some(handler) = handler {
            handler(status);
        }
    }

    /// Observe every session lifecycle event (Connected, Subscribed, Gap, …).
    pub fn on_event(&self, handler: impl Fn(&SessionEvent) + 'static) {
        self.inner.borrow_mut().on_event = Some(Rc::new(handler));
    }

    /// Observe the connection's transport-level liveness (Open / Reconnecting /
    /// Closed) — wire it to a "reconnecting…" indicator.
    pub fn on_status(&self, handler: impl Fn(ConnectionStatus) + 'static) {
        self.inner.borrow_mut().on_status = Some(Rc::new(handler));
    }

    /// Subscribe to `topic` under `subprotocol_id`, routing its Data payloads to
    /// `on_data`. The Subscribe frame is queued until the socket is OPEN, and is
    /// replayed automatically on every reconnect.
    pub fn subscribe(&self, topic: &str, subprotocol_id: u64, on_data: impl Fn(Bytes) + 'static) {
        let mut guard = self.inner.borrow_mut();
        guard.on_data.insert(topic.to_string(), Rc::new(on_data));
        guard
            .subscriptions
            .insert(topic.to_string(), subprotocol_id);
        let frame = guard.session.subscribe(topic, subprotocol_id);
        guard.outbox.push(Outbound::Frame(frame.encode()));
        guard.flush();
    }

    /// Queue `payload` for `topic`; it is sent once the socket is OPEN and the
    /// topic's `SubscribeAck` has bound a ref. Never silently dropped — a payload
    /// queued during a reconnect is sent once the topic re-binds.
    pub fn send_data(&self, topic: &str, subprotocol_id: u64, payload: Bytes) {
        let mut guard = self.inner.borrow_mut();
        guard.queue_data(topic.to_string(), subprotocol_id, payload);
        guard.flush();
    }

    /// The interval (ms) the server asked the client to heartbeat at (0 until
    /// the `Hello` has arrived). The heartbeat is driven automatically.
    pub fn heartbeat_interval_ms(&self) -> u32 {
        self.inner.borrow().session.heartbeat_interval_ms()
    }

    /// Deliberately close the connection and stop reconnecting, emitting a final
    /// [`ConnectionStatus::Closed`]. Idempotent. (Dropping the client tears down
    /// the same way but stays silent — see `Drop`.)
    pub fn close(&self) {
        {
            let mut guard = self.inner.borrow_mut();
            if guard.closed {
                return;
            }
            guard.teardown();
        }
        Self::emit_status(&self.inner, ConnectionStatus::Closed);
    }
}

impl Drop for RealtimeClient {
    fn drop(&mut self) {
        // Silent teardown — the consumer is discarding the client, so there is no
        // one left to observe a `Closed` status (and emitting it while holding the
        // borrow would risk a re-entrant borrow). Use `close()` for an observable
        // shutdown.
        self.inner.borrow_mut().teardown();
    }
}
