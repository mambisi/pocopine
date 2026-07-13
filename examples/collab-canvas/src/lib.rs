#![cfg(pocopine_browser)]
//! Collaborative canvas — a tldraw-style demo of `pocopine-collab`.
//!
//! Two browser sessions (the two iframes in `index.html`) drag the same rects
//! and converge in real time. The point of this example is the **document
//! schema**: a canvas is a *map of objects*, not a sequence. Each rect's
//! position lives under its own key in a `yrs` Map, so two people dragging
//! *different* rects never conflict, and dragging the *same* rect is a clean
//! last-writer-wins on that one key — no delete+insert content loss. (A
//! sequence CRDT like the rich-text `XmlFragment` would be the wrong tool here;
//! see `docs/internal/collab-live-blocks-schema.md`.)
//!
//! Everything below the schema is reused, untouched: the `RealtimeClient`
//! transport, the compatibility-bearing `CollabMessage` hello/SyncStep2/update
//! handshake, and the
//! `CollabSync` server. Only the schema and the (imperative) rendering are
//! app-specific — a canvas is a custom surface, not reactive document DOM.
//!
//! ## Why no self-echo handling is needed
//!
//! The gateway echoes a publish back to its own sender, but re-applying a yrs
//! update is a no-op, and a Map observer only fires on a *real* change — so an
//! echo paints nothing and re-broadcasts nothing. Contrast the rich-text binder,
//! whose coarse whole-doc re-encode needs explicit change-detection. Fine-grained
//! Map updates are naturally clean.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use pocopine_collab::{COLLAB_SUBPROTOCOL, CollabHello, CollabMessage, Presence};
use pocopine_realtime::client::{ConnectionStatus, RealtimeClient, SessionEvent};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{Element, HtmlElement, PointerEvent};
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::Encode;
use yrs::{Any, Doc, Map, MapRef, Observable, Out, ReadTxn, StateVector, Transact, Update};

mod compatibility;

/// The root yrs Map name: shape-id → `{x, y}`.
const POSITIONS: &str = "positions";

/// Per-session cursor colours — a session is assigned one at random so peers can
/// tell each other's cursors apart.
const CURSOR_COLORS: &[&str] = &["#f8ae45", "#e26a48", "#7bb86f", "#6aa0e2", "#c77dff"];

/// A peer's live pointer position on the board — the awareness/presence payload.
/// Ephemeral (relayed, never stored): see `pocopine_collab::Presence`.
#[derive(Serialize, Deserialize, Clone)]
struct Cursor {
    x: f64,
    y: f64,
    color: String,
}

/// Shapes are defined in code — `(id, default x, default y, width, height, fill)`.
/// The doc stores only the collaborative *position overrides*, so there is
/// nothing to seed and therefore no seed-race between joiners.
const SHAPES: &[(&str, f64, f64, f64, f64, &str)] = &[
    ("rect-amber", 40.0, 40.0, 120.0, 84.0, "#f8ae45"),
    ("rect-clay", 240.0, 130.0, 120.0, 84.0, "#e26a48"),
    ("rect-sage", 120.0, 250.0, 120.0, 84.0, "#7bb86f"),
];

/// An in-progress drag: which shape, and the pointer→origin offset.
struct Drag {
    id: String,
    dx: f64,
    dy: f64,
}

/// The whole live session: the CRDT doc, the transport, the board element, and
/// the per-shape divs. Held for the page lifetime in a thread-local.
struct Room {
    doc: Doc,
    positions: MapRef,
    client: Rc<RealtimeClient>,
    topic: String,
    peer_compatible: Cell<bool>,
    board: HtmlElement,
    divs: RefCell<HashMap<String, HtmlElement>>,
    drag: RefCell<Option<Drag>>,
    /// The positions observer subscription — dropped would detach it.
    _sub: RefCell<Option<yrs::Subscription>>,
    /// Ephemeral presence (peer pointer cursors), bound to the same `doc`.
    presence: RefCell<Presence>,
    /// This session's cursor colour.
    cursor_color: String,
    /// The rendered peer-cursor elements, keyed by peer client id.
    cursors: RefCell<HashMap<u64, HtmlElement>>,
}

thread_local! {
    static ROOM: RefCell<Option<Rc<Room>>> = const { RefCell::new(None) };
}

impl Room {
    /// A shape's current position: its override if present, else its code default.
    fn position<T: ReadTxn>(&self, txn: &T, id: &str, default: (f64, f64)) -> (f64, f64) {
        match self.positions.get(txn, id) {
            Some(Out::Any(Any::Map(map))) => (
                num(map.get("x")).unwrap_or(default.0),
                num(map.get("y")).unwrap_or(default.1),
            ),
            _ => default,
        }
    }

    /// Write a shape's position (a local edit) and broadcast just that delta.
    fn set_position(&self, id: &str, x: f64, y: f64) {
        let before = self.doc.transact().state_vector();
        {
            let mut txn = self.doc.transact_mut();
            let mut entry = HashMap::new();
            entry.insert("x".to_string(), Any::Number(x));
            entry.insert("y".to_string(), Any::Number(y));
            self.positions
                .insert(&mut txn, id, Any::Map(Arc::new(entry)));
        } // commit → the observer fires → render
        let update = self.doc.transact().encode_state_as_update_v1(&before);
        if self.peer_compatible.get() {
            self.client.send_data(
                &self.topic,
                COLLAB_SUBPROTOCOL,
                CollabMessage::Update(Bytes::from(update)).encode(),
            );
        }
    }

    /// Publish this session's pointer position as presence (an ephemeral
    /// awareness frame) so peers can draw our cursor.
    fn announce_cursor(&self, x: f64, y: f64) {
        let cursor = Cursor {
            x,
            y,
            color: self.cursor_color.clone(),
        };
        if let Ok(body) = self.presence.borrow_mut().announce(cursor)
            && self.peer_compatible.get()
        {
            self.client.send_data(
                &self.topic,
                COLLAB_SUBPROTOCOL,
                CollabMessage::Awareness(body).encode(),
            );
        }
    }

    /// Stop publishing our cursor (pointer left the board) so peers clear it.
    fn clear_local_cursor(&self) {
        if let Ok(body) = self.presence.borrow_mut().clear_local()
            && self.peer_compatible.get()
        {
            self.client.send_data(
                &self.topic,
                COLLAB_SUBPROTOCOL,
                CollabMessage::Awareness(body).encode(),
            );
        }
    }

    /// Fold a peer's awareness update in, then reconcile the peer-cursor DOM.
    fn apply_awareness(&self, body: &[u8]) {
        if self.presence.borrow_mut().apply(body).is_ok() {
            self.render_cursors();
        }
    }

    /// Create/update/remove a `<div>` per peer cursor to match current presence.
    fn render_cursors(&self) {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let peers: HashMap<u64, Cursor> = {
            let presence = self.presence.borrow();
            let me = presence.client_id();
            presence
                .states::<Cursor>()
                .into_iter()
                .filter(|(id, _)| *id != me)
                .collect()
        };
        let mut cursors = self.cursors.borrow_mut();
        // Drop cursors for peers that have gone away.
        cursors.retain(|id, el| {
            let keep = peers.contains_key(id);
            if !keep {
                el.remove();
            }
            keep
        });
        // Create or move the rest.
        for (id, cursor) in peers {
            let el = cursors
                .entry(id)
                .or_insert_with(|| make_cursor_el(&document, &self.board));
            let style = el.style();
            let _ = style.set_property("left", &format!("{}px", cursor.x));
            let _ = style.set_property("top", &format!("{}px", cursor.y));
            let _ = style.set_property("background", &cursor.color);
        }
    }

    /// Remove every peer cursor (they are stale while we're disconnected).
    fn clear_peer_cursors(&self) {
        for (_, el) in self.cursors.borrow_mut().drain() {
            el.remove();
        }
    }

    /// Paint every shape from `txn` (find-or-create its div, set left/top). Must
    /// read from the supplied transaction — opening a new one inside the Map
    /// observer would panic (a transaction is already active).
    fn render<T: ReadTxn>(&self, txn: &T) {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let mut divs = self.divs.borrow_mut();
        for (id, dx, dy, w, h, fill) in SHAPES.iter().copied() {
            let (x, y) = self.position(txn, id, (dx, dy));
            let el = divs.entry(id.to_string()).or_insert_with(|| {
                let el = document
                    .create_element("div")
                    .unwrap()
                    .dyn_into::<HtmlElement>()
                    .unwrap();
                let style = el.style();
                let _ = el.set_attribute("data-shape-id", id);
                let _ = style.set_property("position", "absolute");
                let _ = style.set_property("width", &format!("{w}px"));
                let _ = style.set_property("height", &format!("{h}px"));
                let _ = style.set_property("background", fill);
                let _ = style.set_property("border-radius", "8px");
                let _ = style.set_property("box-shadow", "0 2px 8px rgba(0,0,0,.28)");
                let _ = style.set_property("cursor", "grab");
                let _ = style.set_property("touch-action", "none");
                let _ = self.board.append_child(&el);
                el
            });
            let style = el.style();
            let _ = style.set_property("left", &format!("{x}px"));
            let _ = style.set_property("top", &format!("{y}px"));
        }
    }
}

/// Extract an `f64` from a yrs `Any` number (integral values arrive as `BigInt`).
fn num(value: Option<&Any>) -> Option<f64> {
    match value {
        Some(Any::Number(n)) => Some(*n),
        Some(Any::BigInt(i)) => Some(*i as f64),
        _ => None,
    }
}

/// A shape's code-defined default position.
fn default_xy(id: &str) -> (f64, f64) {
    SHAPES
        .iter()
        .find(|s| s.0 == id)
        .map(|s| (s.1, s.2))
        .unwrap_or((0.0, 0.0))
}

/// Pointer position relative to the board's top-left.
fn board_local(board: &HtmlElement, event: &PointerEvent) -> (f64, f64) {
    let rect = board.get_bounding_client_rect();
    (
        event.client_x() as f64 - rect.left(),
        event.client_y() as f64 - rect.top(),
    )
}

/// A yrs `client_id` unique per tab (two iframes must not collide, or their
/// concurrent writes would share an id and break convergence). 2^53 of
/// randomness makes a collision astronomically unlikely.
fn random_client_id() -> u64 {
    (js_sys::Math::random() * 9_007_199_254_740_991.0) as u64
}

/// Pick this session's cursor colour at random from the palette.
fn random_cursor_color() -> String {
    let i = (js_sys::Math::random() * CURSOR_COLORS.len() as f64) as usize;
    CURSOR_COLORS[i.min(CURSOR_COLORS.len() - 1)].to_string()
}

/// Create a peer-cursor element (a small teardrop pointer) and mount it on the
/// board. Position/colour are set by the caller per render.
fn make_cursor_el(document: &web_sys::Document, board: &HtmlElement) -> HtmlElement {
    let el = document
        .create_element("div")
        .unwrap()
        .dyn_into::<HtmlElement>()
        .unwrap();
    let style = el.style();
    let _ = style.set_property("position", "absolute");
    let _ = style.set_property("width", "12px");
    let _ = style.set_property("height", "12px");
    // A teardrop: round three corners, point the fourth (rotated) at the spot.
    let _ = style.set_property("border-radius", "50% 50% 50% 0");
    let _ = style.set_property("transform", "rotate(-135deg)");
    let _ = style.set_property("box-shadow", "0 1px 4px rgba(0,0,0,.45)");
    let _ = style.set_property("pointer-events", "none");
    let _ = style.set_property("z-index", "3");
    let _ = board.append_child(&el);
    el
}

/// Wire the collab sync handshake to the transport.
fn install_handshake(room: &Rc<Room>) {
    // On subscribe-ack, restart compatibility negotiation and announce our
    // state vector. Local edits made before the reply remain in yrs and are
    // uploaded by the requested SyncStep2 rather than sent pre-hello.
    {
        let weak = Rc::downgrade(room);
        let subscribed_topic = room.topic.clone();
        room.client.on_event(move |event| {
            if let SessionEvent::Subscribed { topic, .. } = event
                && topic == &subscribed_topic
                && let Some(room) = weak.upgrade()
            {
                room.peer_compatible.set(false);
                let sv = room.doc.transact().state_vector().encode_v1();
                room.client.send_data(
                    &room.topic,
                    COLLAB_SUBPROTOCOL,
                    CollabMessage::Hello(CollabHello::new(
                        compatibility::identity(),
                        Bytes::from(sv),
                        true,
                    ))
                    .encode(),
                );
            }
        });
    }

    // Validate the server hello before answering it or applying any yrs bytes.
    {
        let weak = Rc::downgrade(room);
        let topic = room.topic.clone();
        room.client
            .subscribe(&topic, COLLAB_SUBPROTOCOL, move |payload| {
                let Some(room) = weak.upgrade() else { return };
                let message = match CollabMessage::decode(&payload) {
                    Ok(message) => message,
                    Err(_) => {
                        room.peer_compatible.set(false);
                        return;
                    }
                };
                match message {
                    CollabMessage::Hello(hello) => {
                        room.peer_compatible.set(false);
                        if hello.compatibility() != &compatibility::identity() {
                            return;
                        }
                        let Ok(sv) = StateVector::decode_v1(hello.state_vector()) else {
                            return;
                        };
                        let diff = room.doc.transact().encode_state_as_update_v1(&sv);
                        room.peer_compatible.set(true);
                        if hello.requests_sync_step2() {
                            room.client.send_data(
                                &room.topic,
                                COLLAB_SUBPROTOCOL,
                                CollabMessage::SyncStep2(Bytes::from(diff)).encode(),
                            );
                        }
                    }
                    CollabMessage::SyncStep2(update) | CollabMessage::Update(update) => {
                        if !room.peer_compatible.get() {
                            return;
                        }
                        if let Ok(update) = Update::decode_v1(&update) {
                            let mut txn = room.doc.transact_mut();
                            let _ = txn.apply_update(update);
                        }
                    }
                    CollabMessage::Awareness(body) => {
                        if room.peer_compatible.get() {
                            room.apply_awareness(&body);
                        }
                    }
                }
            });
    }
}

/// Reflect the transport's connection liveness in the `#status` pill, so a
/// dropped-then-recovered session (e.g. after the tab was backgrounded long
/// enough for the server to reap it) is visible. The transport reconnects and
/// re-subscribes on its own; this only mirrors the state.
fn install_status(room: &Rc<Room>, document: &web_sys::Document) {
    let Some(pill) = document
        .get_element_by_id("status")
        .and_then(|el| el.dyn_into::<HtmlElement>().ok())
    else {
        return;
    };
    let weak = Rc::downgrade(room);
    room.client.on_status(move |status| {
        let (text, color) = match status {
            ConnectionStatus::Open => ("● live", "#7bb86f"),
            ConnectionStatus::Reconnecting => ("○ reconnecting…", "#e26a48"),
            ConnectionStatus::Closed => ("", "transparent"),
        };
        pill.set_text_content(Some(text));
        let style = pill.style();
        let _ = style.set_property("color", color);
        let _ = style.set_property("opacity", if text.is_empty() { "0" } else { "1" });
        // Peer cursors go stale while disconnected — drop them until we're live
        // again (peers re-announce on their next pointer move).
        if !matches!(status, ConnectionStatus::Open)
            && let Some(room) = weak.upgrade()
        {
            room.clear_peer_cursors();
        }
    });
}

/// Wire pointer drag on the board: down picks a shape, move writes its position,
/// up ends the drag. Writes go to the CRDT, which repaints + broadcasts.
fn install_pointer(room: &Rc<Room>) {
    {
        let handle = room.clone();
        let down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            let Some(id) = event
                .target()
                .and_then(|t| t.dyn_into::<Element>().ok())
                .and_then(|el| el.get_attribute("data-shape-id"))
            else {
                return;
            };
            let (px, py) = board_local(&handle.board, &event);
            let (x, y) = {
                let txn = handle.doc.transact();
                handle.position(&txn, &id, default_xy(&id))
            };
            *handle.drag.borrow_mut() = Some(Drag {
                id,
                dx: px - x,
                dy: py - y,
            });
            let _ = handle.board.set_pointer_capture(event.pointer_id());
            event.prevent_default();
        });
        room.board
            .set_onpointerdown(Some(down.as_ref().unchecked_ref()));
        down.forget();
    }
    {
        let handle = room.clone();
        let move_ = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
            let (px, py) = board_local(&handle.board, &event);
            // Broadcast our pointer position as presence every move (drag or not).
            handle.announce_cursor(px, py);
            // Compute the new shape position and release the drag borrow BEFORE writing.
            let next = {
                let drag = handle.drag.borrow();
                drag.as_ref()
                    .map(|d| (d.id.clone(), (px - d.dx).max(0.0), (py - d.dy).max(0.0)))
            };
            if let Some((id, x, y)) = next {
                handle.set_position(&id, x, y);
            }
        });
        room.board
            .set_onpointermove(Some(move_.as_ref().unchecked_ref()));
        move_.forget();
    }
    {
        let handle = room.clone();
        let up = Closure::<dyn FnMut(PointerEvent)>::new(move |_event: PointerEvent| {
            *handle.drag.borrow_mut() = None;
        });
        room.board
            .set_onpointerup(Some(up.as_ref().unchecked_ref()));
        up.forget();
    }
    {
        // Pointer left the board — stop publishing our cursor so peers drop it.
        let handle = room.clone();
        let leave = Closure::<dyn FnMut(PointerEvent)>::new(move |_event: PointerEvent| {
            handle.clear_local_cursor();
        });
        room.board
            .set_onpointerleave(Some(leave.as_ref().unchecked_ref()));
        leave.forget();
    }
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let board = document
        .get_element_by_id("board")
        .ok_or_else(|| JsValue::from_str("missing #board element"))?
        .dyn_into::<HtmlElement>()?;

    let location = window.location();
    let scheme = if location.protocol().unwrap_or_default() == "https:" {
        "wss"
    } else {
        "ws"
    };
    let host = location
        .host()
        .unwrap_or_else(|_| "127.0.0.1:3030".to_string());
    let url = format!("{scheme}://{host}{}", pocopine_realtime::WS_STREAM_PATH);

    let client = Rc::new(RealtimeClient::connect(&url)?);
    let doc = Doc::with_client_id(random_client_id());
    let positions = doc.get_or_insert_map(POSITIONS);
    // Presence shares the document's yrs client id, so each tab's cursor is keyed
    // by the same id its edits carry.
    let presence = Presence::new(doc.clone());

    let room = Rc::new(Room {
        doc,
        positions,
        client,
        topic: compatibility::topic(),
        peer_compatible: Cell::new(false),
        board,
        divs: RefCell::new(HashMap::new()),
        drag: RefCell::new(None),
        _sub: RefCell::new(None),
        presence: RefCell::new(presence),
        cursor_color: random_cursor_color(),
        cursors: RefCell::new(HashMap::new()),
    });

    // Repaint on any position change — a local drag OR a merged remote update.
    {
        let weak = Rc::downgrade(&room);
        let sub = room.positions.observe(move |txn, _event| {
            if let Some(room) = weak.upgrade() {
                room.render(txn);
            }
        });
        *room._sub.borrow_mut() = Some(sub);
    }

    install_handshake(&room);
    install_status(&room, &document);
    install_pointer(&room);

    {
        let txn = room.doc.transact();
        room.render(&txn);
    }

    ROOM.with(|cell| *cell.borrow_mut() = Some(room));
    Ok(())
}
