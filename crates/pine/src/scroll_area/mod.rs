//! `<pine-scroll-area-*>` — custom-scrollbar scroll area (compound).
//!
//! Five parts, matching Radix-Vue's ScrollArea anatomy:
//!
//! - **Root** (`pine-scroll-area-root`) — owns the geometry state
//!   (`scroll_top`, `scroll_height`, `client_height`, …) and the
//!   per-root runtime (fade timer + drag state). Provides its Handle.
//!   `type` prop picks the visibility model: `"auto" | "always" |
//!   "scroll" | "hover"`.
//! - **Viewport** (`pine-scroll-area-viewport`) — scrollable host.
//!   Its inner `<div pp-ref="viewport">` carries the overflow +
//!   hidden-native-scrollbar inline styles and listens for
//!   `scroll` / resize so Root's geometry fields stay fresh.
//! - **Scrollbar** (`pine-scroll-area-scrollbar`) — rail for one
//!   axis. `orientation` prop (`"vertical"` (default) |
//!   `"horizontal"`). `data-state="visible" | "hidden"` drives
//!   author CSS. Provides its orientation to Thumb.
//! - **Thumb** (`pine-scroll-area-thumb`) — draggable handle inside
//!   the rail. Size + offset computed from Root geometry, emitted
//!   as inline style bindings so no CSS is needed to position it.
//! - **Corner** (`pine-scroll-area-corner`) — the rectangle in the
//!   bottom-right where two visible scrollbars meet. Auto-hidden
//!   when only one axis has overflow.
//!
//! ```html
//! <pine-scroll-area-root type="hover">
//!   <pine-scroll-area-viewport>
//!     <!-- your long content here -->
//!   </pine-scroll-area-viewport>
//!   <pine-scroll-area-scrollbar orientation="vertical">
//!     <pine-scroll-area-thumb></pine-scroll-area-thumb>
//!   </pine-scroll-area-scrollbar>
//!   <pine-scroll-area-scrollbar orientation="horizontal">
//!     <pine-scroll-area-thumb></pine-scroll-area-thumb>
//!   </pine-scroll-area-scrollbar>
//!   <pine-scroll-area-corner></pine-scroll-area-corner>
//! </pine-scroll-area-root>
//! ```

use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, refs};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{EventTarget, PointerEvent};

create_context!(ROOT: Handle<PineScrollAreaRoot>);
create_context!(SCROLLBAR: Handle<PineScrollAreaScrollbar>);
create_context!(SCROLLBAR_ORIENTATION: String);

// Per-Root runtime: captures the fade-hide timer + Thumb drag state
// so `on_unmount` can tear everything down without leaving
// document-level listeners or pending timeouts behind.
struct RootRuntime {
    viewport_el: Option<web_sys::Element>,
    /// `setTimeout` handle used by `type="scroll"` / `type="hover"`
    /// to flip `scrolling` back to false after `scroll_hide_delay`.
    hide_timer: Option<i32>,
    // Document-level pointer listeners installed for the currently-
    // active Thumb drag (one at most). `on_unmount` detaches these.
    pointer_move: Option<Closure<dyn FnMut(PointerEvent)>>,
    pointer_up: Option<Closure<dyn FnMut(PointerEvent)>>,
    dragging_pointer_id: Option<i32>,
}

thread_local! {
    static ROOT_RUNTIME: RefCell<HashMap<ScopeId, RootRuntime>> =
        RefCell::new(HashMap::new());
}

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineScrollAreaRoot.poco", role = "scope")]
// RFC 049 — a ScrollArea composes a Viewport (the scrollable
// surface), Scrollbar(s) (usually two: horizontal + vertical),
// and an optional Corner (for the small square where the two
// scrollbars meet).
#[slot(default, only = [
    PineScrollAreaViewport,
    PineScrollAreaScrollbar,
    PineScrollAreaCorner,
])]
pub struct PineScrollAreaRoot {
    /// Visibility model for the custom scrollbars. See the module
    /// docs for semantics.
    #[prop]
    pub r#type: String,
    /// Delay (ms) before `type="scroll"` / `type="hover"` fade out
    /// after the last scroll / pointer event.
    #[prop]
    pub scroll_hide_delay: u32,

    // Live geometry mirrored from the Viewport's inner scrollable
    // element. Children observe these via `#[observe(ROOT)]`.
    pub scroll_top: f64,
    pub scroll_left: f64,
    pub scroll_height: f64,
    pub scroll_width: f64,
    pub client_height: f64,
    pub client_width: f64,

    /// `scroll_height > client_height` — drives
    /// `data-has-overflow-y` on Root + the vertical Scrollbar's
    /// mount state (for `type="auto"` / `"scroll"`).
    pub has_vertical: bool,
    pub has_horizontal: bool,

    /// Whether each axis's scrollbar should currently be visible.
    /// For `type="always"` this stays true whenever there's
    /// overflow; for `type="auto"` it mirrors `has_*`; for
    /// `type="scroll" | "hover"` it goes true during scroll/hover
    /// + flips back false after `scroll_hide_delay`.
    pub v_visible: bool,
    pub h_visible: bool,

    /// Transient: true while the user is actively scrolling (or,
    /// for `type="hover"`, while pointer is over Root). Drives the
    /// fade-out timer.
    pub scrolling: bool,
    /// Transient: true while pointer is over Root. Only meaningful
    /// for `type="hover"`.
    pub hovered: bool,
}

impl Default for PineScrollAreaRoot {
    fn default() -> Self {
        Self {
            r#type: "hover".into(),
            scroll_hide_delay: 600,
            scroll_top: 0.0,
            scroll_left: 0.0,
            scroll_height: 0.0,
            scroll_width: 0.0,
            client_height: 0.0,
            client_width: 0.0,
            has_vertical: false,
            has_horizontal: false,
            v_visible: false,
            h_visible: false,
            scrolling: false,
            hovered: false,
        }
    }
}

#[handlers]
impl PineScrollAreaRoot {
    pub fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            teardown_runtime(scope);
        }
    }

    /// Called by Root's own `@pointerenter` / `@pointerleave`
    /// template hooks. Only meaningful for `type="hover"` — other
    /// types ignore `hovered` in `recompute_visibility`.
    pub fn on_pointer_enter(&mut self) {
        if !self.hovered {
            self.hovered = true;
            self.recompute_visibility();
        }
    }

    pub fn on_pointer_leave(&mut self) {
        if self.hovered {
            self.hovered = false;
            self.recompute_visibility();
        }
    }
}

impl PineScrollAreaRoot {
    /// Absorb a scroll event from the Viewport inner element.
    /// Updates `scroll_top`/`scroll_left` + sets `scrolling=true`
    /// so the fade timer can flip it back false later.
    pub fn on_scroll(&mut self, scroll_top: f64, scroll_left: f64) {
        self.scroll_top = scroll_top;
        self.scroll_left = scroll_left;
        self.scrolling = true;
        self.recompute_visibility();
    }

    /// Absorb a resize / geometry change. `scroll_*` and `client_*`
    /// come from `viewport.scrollHeight` + `viewport.clientHeight`
    /// (and the horizontal analogues). Recomputes `has_*` +
    /// visibility.
    pub fn on_geometry(
        &mut self,
        scroll_height: f64,
        scroll_width: f64,
        client_height: f64,
        client_width: f64,
    ) {
        self.scroll_height = scroll_height;
        self.scroll_width = scroll_width;
        self.client_height = client_height;
        self.client_width = client_width;
        self.has_vertical = scroll_height > client_height + 0.5;
        self.has_horizontal = scroll_width > client_width + 0.5;
        self.recompute_visibility();
    }

    /// Called by the fade timer when the inactivity window elapses.
    pub fn end_scroll(&mut self) {
        if self.scrolling {
            self.scrolling = false;
            self.recompute_visibility();
        }
    }

    fn recompute_visibility(&mut self) {
        let (v, h) = match self.r#type.as_str() {
            "always" => (self.has_vertical, self.has_horizontal),
            "auto" => (self.has_vertical, self.has_horizontal),
            "hover" => (
                self.has_vertical && (self.hovered || self.scrolling),
                self.has_horizontal && (self.hovered || self.scrolling),
            ),
            // default + "scroll"
            _ => (
                self.has_vertical && self.scrolling,
                self.has_horizontal && self.scrolling,
            ),
        };
        self.v_visible = v;
        self.h_visible = h;
    }
}

// ── Viewport ──────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineScrollAreaViewport.poco", role = "panel")]
pub struct PineScrollAreaViewport {}

#[handlers]
impl PineScrollAreaViewport {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(viewport_el) = refs.get("viewport") else {
            return;
        };
        let Some(root) = ROOT.inject() else {
            return;
        };
        ROOT_RUNTIME.with(|r| {
            let mut map = r.borrow_mut();
            let entry = map.entry(root.scope_id()).or_insert_with(|| RootRuntime {
                viewport_el: None,
                hide_timer: None,
                pointer_move: None,
                pointer_up: None,
                dragging_pointer_id: None,
            });
            entry.viewport_el = Some(viewport_el.clone());
        });
        // Push initial geometry so the scrollbars can compute their
        // thumb sizing before the user touches anything.
        push_geometry(&viewport_el, &root);
    }

    /// `@scroll` handler on the inner scrollable div.
    pub fn on_scroll(&mut self) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(viewport_el) = refs::get_on(scope, "viewport") else {
            return;
        };
        let top = js_scroll_top(&viewport_el);
        let left = js_scroll_left(&viewport_el);
        root.update(|r: &mut PineScrollAreaRoot| r.on_scroll(top, left));
        schedule_fade(root);
    }

    /// `pp-resize` handler — fires whenever the Viewport's size
    /// changes. Refresh geometry + push to Root.
    pub fn on_resize(&mut self, _w: f64, _h: f64) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(viewport_el) = refs::get_on(scope, "viewport") else {
            return;
        };
        push_geometry(&viewport_el, &root);
    }
}

fn js_scroll_top(el: &web_sys::Element) -> f64 {
    js_sys::Reflect::get(el.as_ref(), &wasm_bindgen::JsValue::from_str("scrollTop"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn js_scroll_left(el: &web_sys::Element) -> f64 {
    js_sys::Reflect::get(el.as_ref(), &wasm_bindgen::JsValue::from_str("scrollLeft"))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn read_f64(el: &web_sys::Element, prop: &str) -> f64 {
    js_sys::Reflect::get(el.as_ref(), &wasm_bindgen::JsValue::from_str(prop))
        .ok()
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
}

fn push_geometry(viewport_el: &web_sys::Element, root: &Handle<PineScrollAreaRoot>) {
    let sh = read_f64(viewport_el, "scrollHeight");
    let sw = read_f64(viewport_el, "scrollWidth");
    let ch = read_f64(viewport_el, "clientHeight");
    let cw = read_f64(viewport_el, "clientWidth");
    let st = js_scroll_top(viewport_el);
    let sl = js_scroll_left(viewport_el);
    root.update(move |r: &mut PineScrollAreaRoot| {
        r.on_geometry(sh, sw, ch, cw);
        r.scroll_top = st;
        r.scroll_left = sl;
    });
}

fn schedule_fade(root: Handle<PineScrollAreaRoot>) {
    let scope = root.scope_id();
    let (kind, delay) = root.with(|r| (r.r#type.clone(), r.scroll_hide_delay));
    // `auto` / `always` never fade on inactivity — `scrolling` can
    // stay true, it doesn't affect their visibility computation.
    if kind == "auto" || kind == "always" {
        return;
    }
    let Some(window) = web_sys::window() else {
        return;
    };
    // Cancel any in-flight timer first so rapid scrolls don't stack.
    ROOT_RUNTIME.with(|r| {
        if let Some(rt) = r.borrow_mut().get_mut(&scope) {
            if let Some(prev) = rt.hide_timer.take() {
                window.clear_timeout_with_handle(prev);
            }
        }
    });
    let root_for_cb = root;
    let cb = Closure::once_into_js(move || {
        root_for_cb.update(|r: &mut PineScrollAreaRoot| r.end_scroll());
    });
    let handle = window
        .set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            delay as i32,
        )
        .unwrap_or(0);
    ROOT_RUNTIME.with(|r| {
        if let Some(rt) = r.borrow_mut().get_mut(&scope) {
            rt.hide_timer = Some(handle);
        }
    });
}

// ── Scrollbar ─────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineScrollAreaScrollbar.poco", role = "panel")]
// RFC 049 — a Scrollbar contains exactly its Thumb. The
// Thumb's position is the visual indicator of scroll
// progress; nothing else belongs inside the bar.
#[slot(default, only = [PineScrollAreaThumb])]
pub struct PineScrollAreaScrollbar {
    /// `"vertical"` (default) or `"horizontal"`.
    #[prop]
    pub orientation: String,
    /// Mirrored from Root for this axis — drives the mount gate
    /// (`pp-if`) + `data-state`.
    pub visible: bool,
    pub has_overflow: bool,
}

impl Default for PineScrollAreaScrollbar {
    fn default() -> Self {
        Self {
            orientation: "vertical".into(),
            visible: false,
            has_overflow: false,
        }
    }
}

#[handlers]
impl PineScrollAreaScrollbar {
    pub fn on_setup(&mut self) {
        SCROLLBAR.provide(this::<Self>());
        SCROLLBAR_ORIENTATION.provide(self.orientation.clone());
        if let Some(root) = ROOT.inject() {
            let (v_vis, h_vis, has_v, has_h) =
                root.with(|r| (r.v_visible, r.h_visible, r.has_vertical, r.has_horizontal));
            if self.orientation == "horizontal" {
                self.visible = h_vis;
                self.has_overflow = has_h;
            } else {
                self.visible = v_vis;
                self.has_overflow = has_v;
            }
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let root_scope = root.scope_id();
        let orientation = self.orientation.clone();

        // Mirror v_visible / h_visible based on this scrollbar's axis.
        let h1 = handle.clone();
        let o1 = orientation.clone();
        let r1 = root.clone();
        pocopine::watch_scope_field::<bool, _>(root_scope, "v_visible", move |&v, _| {
            if o1 == "vertical" {
                let has = r1.with(|r| r.has_vertical);
                h1.update(|s| {
                    s.visible = v;
                    s.has_overflow = has;
                });
            }
        });
        let h2 = handle.clone();
        let o2 = orientation.clone();
        let r2 = root.clone();
        pocopine::watch_scope_field::<bool, _>(root_scope, "h_visible", move |&v, _| {
            if o2 == "horizontal" {
                let has = r2.with(|r| r.has_horizontal);
                h2.update(|s| {
                    s.visible = v;
                    s.has_overflow = has;
                });
            }
        });
        // Track has_* too so `data-has-overflow` refreshes when
        // content size changes but visibility (for type="always")
        // doesn't toggle.
        let h3 = handle.clone();
        let o3 = orientation.clone();
        pocopine::watch_scope_field::<bool, _>(root_scope, "has_vertical", move |&v, _| {
            if o3 == "vertical" {
                h3.update(|s| s.has_overflow = v);
            }
        });
        let h4 = handle;
        let o4 = orientation;
        pocopine::watch_scope_field::<bool, _>(root_scope, "has_horizontal", move |&v, _| {
            if o4 == "horizontal" {
                h4.update(|s| s.has_overflow = v);
            }
        });
    }
}

// ── Thumb ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineScrollAreaThumb.poco", role = "interactive")]
pub struct PineScrollAreaThumb {
    /// Inherited from the enclosing Scrollbar so this Thumb knows
    /// which axis to drag.
    pub orientation: String,
    /// Thumb size as a percentage of the rail
    /// (`client_* / scroll_* * 100`). Clamped to `[0, 100]`.
    pub size_pct: f64,
    /// Thumb offset from the rail's top/left as a percentage
    /// (`scroll_* / scroll_* * 100`). Clamped to `[0, 100 - size_pct]`.
    pub offset_pct: f64,
}

#[handlers]
impl PineScrollAreaThumb {
    pub fn on_setup(&mut self) {
        self.orientation = SCROLLBAR_ORIENTATION
            .inject()
            .unwrap_or_else(|| "vertical".into());
        if let Some(root) = ROOT.inject() {
            let (sh, sw, ch, cw, st, sl) = root.with(|r| {
                (
                    r.scroll_height,
                    r.scroll_width,
                    r.client_height,
                    r.client_width,
                    r.scroll_top,
                    r.scroll_left,
                )
            });
            let (size, offset) = if self.orientation == "horizontal" {
                thumb_metrics(cw, sw, sl)
            } else {
                thumb_metrics(ch, sh, st)
            };
            self.size_pct = size;
            self.offset_pct = offset;
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let root_scope = root.scope_id();
        let orientation = self.orientation.clone();
        let root_for_watch = root.clone();
        let handle_for_watch = handle.clone();
        let recompute = move || {
            let (sh, sw, ch, cw, st, sl) = root_for_watch.with(|r| {
                (
                    r.scroll_height,
                    r.scroll_width,
                    r.client_height,
                    r.client_width,
                    r.scroll_top,
                    r.scroll_left,
                )
            });
            let (size, offset) = if orientation == "horizontal" {
                thumb_metrics(cw, sw, sl)
            } else {
                thumb_metrics(ch, sh, st)
            };
            handle_for_watch.update(|s| {
                s.size_pct = size;
                s.offset_pct = offset;
            });
        };
        // Any of the six geometry fields can change thumb sizing.
        // Resubscribing to each keeps the code obvious (vs a single
        // computed-like fan-in) and the watches are cheap.
        for field in [
            "scroll_top",
            "scroll_left",
            "scroll_height",
            "scroll_width",
            "client_height",
            "client_width",
        ] {
            let r = recompute.clone();
            pocopine::watch_scope_field::<f64, _>(root_scope, field, move |_, _| r());
        }
    }

    /// `@pointerdown` — start a drag. The rest (move / up) attach to
    /// `document` so the drag doesn't drop when the pointer leaves
    /// the thumb.
    pub fn on_pointer_down(&mut self, ev: wasm_bindgen::JsValue) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let Some(scrollbar) = SCROLLBAR.inject() else {
            return;
        };
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(thumb_el) = refs::get_on(scope, "thumb") else {
            return;
        };
        let Some(rail_el) = refs::get_on(scrollbar.scope_id(), "rail") else {
            return;
        };
        let Ok(ev) = ev.dyn_into::<PointerEvent>() else {
            return;
        };
        let orientation = self.orientation.clone();
        start_drag(root, rail_el, thumb_el, orientation, ev);
    }
}

fn thumb_metrics(client: f64, scroll: f64, offset: f64) -> (f64, f64) {
    if scroll <= 0.0 {
        return (0.0, 0.0);
    }
    let size = (client / scroll * 100.0).clamp(0.0, 100.0);
    let off = (offset / scroll * 100.0).clamp(0.0, 100.0 - size);
    (size, off)
}

fn start_drag(
    root: Handle<PineScrollAreaRoot>,
    rail_el: web_sys::Element,
    thumb_el: web_sys::Element,
    orientation: String,
    ev: PointerEvent,
) {
    let root_scope = root.scope_id();
    let Some(viewport_el) = ROOT_RUNTIME.with(|r| {
        r.borrow()
            .get(&root_scope)
            .and_then(|rt| rt.viewport_el.clone())
    }) else {
        return;
    };
    let rail_rect = rail_el.get_bounding_client_rect();

    // Anchor: pointer position vs. thumb offset within the rail, so
    // the thumb doesn't snap to the pointer origin — the delta the
    // user drags maps 1:1 into rail percentage.
    let thumb_rect = thumb_el.get_bounding_client_rect();
    let (pointer_origin, thumb_origin) = if orientation == "horizontal" {
        (ev.client_x() as f64, thumb_rect.left())
    } else {
        (ev.client_y() as f64, thumb_rect.top())
    };
    let grab_offset = pointer_origin - thumb_origin;

    ev.prevent_default();
    ROOT_RUNTIME.with(|r| {
        if let Some(rt) = r.borrow_mut().get_mut(&root_scope) {
            rt.dragging_pointer_id = Some(ev.pointer_id());
        }
    });

    let orient_for_move = orientation.clone();
    let rail_rect_for_move = rail_rect;
    let rail_el_for_move = rail_el.clone();
    let viewport_for_move = viewport_el.clone();
    let root_for_move = root.clone();
    let move_ = Closure::wrap(Box::new(move |ev: PointerEvent| {
        let active = ROOT_RUNTIME.with(|r| {
            r.borrow()
                .get(&root_scope)
                .and_then(|rt| rt.dragging_pointer_id)
                == Some(ev.pointer_id())
        });
        if !active {
            return;
        }
        ev.prevent_default();
        let (sh, sw, ch, cw) = root_for_move.with(|r| {
            (
                r.scroll_height,
                r.scroll_width,
                r.client_height,
                r.client_width,
            )
        });
        // Recompute rail size from the current client rect. The
        // rail can resize mid-drag (content changes, window resize)
        // so the cached rect is only a seed for the grab offset.
        let rail_now = rail_el_for_move.get_bounding_client_rect();
        if orient_for_move == "horizontal" {
            let rail_len = rail_now.width().max(1.0);
            let pointer = (ev.client_x() as f64) - rail_now.left();
            let new_thumb_left = (pointer - grab_offset).max(0.0);
            let max_scroll = (sw - cw).max(0.0);
            // Available travel for the thumb = rail length - thumb size.
            let thumb_size = (cw / sw * rail_len).clamp(0.0, rail_len);
            let travel = (rail_len - thumb_size).max(1.0);
            let new_scroll = (new_thumb_left / travel) * max_scroll;
            set_scroll_left(&viewport_for_move, new_scroll.clamp(0.0, max_scroll));
        } else {
            let rail_len = rail_now.height().max(1.0);
            let pointer = (ev.client_y() as f64) - rail_now.top();
            let new_thumb_top = (pointer - grab_offset).max(0.0);
            let max_scroll = (sh - ch).max(0.0);
            let thumb_size = (ch / sh * rail_len).clamp(0.0, rail_len);
            let travel = (rail_len - thumb_size).max(1.0);
            let new_scroll = (new_thumb_top / travel) * max_scroll;
            set_scroll_top(&viewport_for_move, new_scroll.clamp(0.0, max_scroll));
        }
        // The `scroll` event from the viewport flows back through
        // Viewport::on_scroll and updates Root.scroll_*, which in
        // turn fans out to the Thumb's watches. No need to
        // handle.update here.
        let _ = &rail_rect_for_move;
    }) as Box<dyn FnMut(PointerEvent)>);

    let up = Closure::wrap(Box::new(move |ev: PointerEvent| {
        ROOT_RUNTIME.with(|r| {
            let mut map = r.borrow_mut();
            if let Some(rt) = map.get_mut(&root_scope) {
                if rt.dragging_pointer_id == Some(ev.pointer_id()) {
                    rt.dragging_pointer_id = None;
                }
            }
        });
    }) as Box<dyn FnMut(PointerEvent)>);

    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let t: &EventTarget = doc.as_ref();
        let _ = t.add_event_listener_with_callback("pointermove", move_.as_ref().unchecked_ref());
        let _ = t.add_event_listener_with_callback("pointerup", up.as_ref().unchecked_ref());
        let _ = t.add_event_listener_with_callback("pointercancel", up.as_ref().unchecked_ref());
    }

    ROOT_RUNTIME.with(|r| {
        if let Some(rt) = r.borrow_mut().get_mut(&root_scope) {
            rt.pointer_move = Some(move_);
            rt.pointer_up = Some(up);
        }
    });
}

fn set_scroll_top(viewport: &web_sys::Element, v: f64) {
    let _ = js_sys::Reflect::set(
        viewport.as_ref(),
        &wasm_bindgen::JsValue::from_str("scrollTop"),
        &wasm_bindgen::JsValue::from_f64(v),
    );
}

fn set_scroll_left(viewport: &web_sys::Element, v: f64) {
    let _ = js_sys::Reflect::set(
        viewport.as_ref(),
        &wasm_bindgen::JsValue::from_str("scrollLeft"),
        &wasm_bindgen::JsValue::from_f64(v),
    );
}

// ── Corner ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineScrollAreaCorner.poco", role = "visual")]
pub struct PineScrollAreaCorner {
    pub show: bool,
}

#[handlers]
impl PineScrollAreaCorner {
    pub fn on_setup(&mut self) {
        if let Some(root) = ROOT.inject() {
            self.show = root.with(|r| r.v_visible && r.h_visible);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let root_scope = root.scope_id();
        let recompute = move |root: Handle<PineScrollAreaRoot>, h: Handle<Self>| {
            let show = root.with(|r| r.v_visible && r.h_visible);
            h.update(|s| s.show = show);
        };
        let r1 = root.clone();
        let h1 = handle.clone();
        let rc = recompute;
        pocopine::watch_scope_field::<bool, _>(root_scope, "v_visible", move |_, _| {
            rc(r1.clone(), h1.clone());
        });
        let r2 = root;
        let h2 = handle;
        pocopine::watch_scope_field::<bool, _>(root_scope, "h_visible", move |_, _| {
            recompute(r2.clone(), h2.clone());
        });
    }
}

// ── Teardown ──────────────────────────────────────────────────────

fn teardown_runtime(scope: ScopeId) {
    let rt = match ROOT_RUNTIME.with(|r| r.borrow_mut().remove(&scope)) {
        Some(rt) => rt,
        None => return,
    };
    if let Some(handle) = rt.hide_timer {
        if let Some(win) = web_sys::window() {
            win.clear_timeout_with_handle(handle);
        }
    }
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let t: &EventTarget = doc.as_ref();
        if let Some(c) = rt.pointer_move.as_ref() {
            let _ =
                t.remove_event_listener_with_callback("pointermove", c.as_ref().unchecked_ref());
        }
        if let Some(c) = rt.pointer_up.as_ref() {
            let _ = t.remove_event_listener_with_callback("pointerup", c.as_ref().unchecked_ref());
            let _ =
                t.remove_event_listener_with_callback("pointercancel", c.as_ref().unchecked_ref());
        }
    }
}
