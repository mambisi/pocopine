//! `pp-anchor[:<placement>][.<modifier>...]="<anchor>"` — RFC-015.
//!
//! Position a floating element relative to an anchor via JS layout
//! math. Works in every major browser today; no reliance on the
//! still-in-flux CSS Anchor Positioning spec.
//!
//! The floater gets `position: fixed` with computed `top` + `left`.
//! Scroll, resize, and ResizeObserver callbacks recompute on change.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    AddEventListenerOptions, Element, Event, EventTarget, HtmlElement, ResizeObserver,
};

use super::DirectiveCall;
use crate::refs;

const STATE_KEY: &str = "__pp_anchor_state";

/// Which side of the anchor the floater sits on.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Side {
    Top,
    Bottom,
    Left,
    Right,
}

/// Cross-axis alignment of the floater against the anchor.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Align {
    Start,
    Center,
    End,
}

#[derive(Copy, Clone)]
struct Placement {
    side: Side,
    align: Align,
}

impl Placement {
    fn opposite(self) -> Self {
        Self {
            side: match self.side {
                Side::Top => Side::Bottom,
                Side::Bottom => Side::Top,
                Side::Left => Side::Right,
                Side::Right => Side::Left,
            },
            align: self.align,
        }
    }
}

pub fn run(call: &DirectiveCall) {
    let Some(anchor) = resolve_anchor(call) else {
        return;
    };

    let floater: HtmlElement = match call.el.clone().dyn_into() {
        Ok(h) => h,
        Err(_) => return,
    };

    let placement = parse_placement(call.arg.as_deref());
    let offset = parse_offset(&call.modifiers);
    let flip = call.modifiers.iter().any(|m| m == "flip");

    // Hold everything the recompute path needs in one Rc<RefCell<_>>
    // so closures can share state without juggling multiple Rcs.
    let inner = Rc::new(RefCell::new(AnchorInner {
        anchor,
        floater: floater.clone(),
        placement,
        offset,
        flip,
    }));

    // The reposition function every trigger calls into.
    let reposition_closure: Closure<dyn FnMut()> = {
        let inner = inner.clone();
        Closure::wrap(Box::new(move || {
            let guard = inner.borrow();
            reposition(&guard);
        }) as Box<dyn FnMut()>)
    };
    let reposition_fn: Function = reposition_closure.as_ref().unchecked_ref::<Function>().clone();

    // Kick off once on the next microtask so the floater has a chance
    // to commit its initial measured size.
    crate::tick::next({
        let f = reposition_fn.clone();
        move || {
            let _ = f.call0(&JsValue::UNDEFINED);
        }
    });

    // ResizeObservers on both the anchor and the floater — either
    // side resizing invalidates the layout.
    let obs_callback: Closure<dyn FnMut(JsValue, JsValue)> = {
        let f = reposition_fn.clone();
        Closure::wrap(Box::new(move |_entries: JsValue, _obs: JsValue| {
            let _ = f.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut(JsValue, JsValue)>)
    };
    let obs_cb_fn = obs_callback.as_ref().unchecked_ref::<Function>().clone();

    let anchor_observer = match ResizeObserver::new(&obs_cb_fn) {
        Ok(o) => {
            o.observe(&inner.borrow().anchor);
            Some(o)
        }
        Err(_) => None,
    };
    let floater_observer = match ResizeObserver::new(&obs_cb_fn) {
        Ok(o) => {
            o.observe(inner.borrow().floater.as_ref());
            Some(o)
        }
        Err(_) => None,
    };

    // Window scroll + resize. `scroll` uses capture so nested
    // scrollers still trigger — any ancestor scrolling the anchor
    // into a new position needs to re-run the layout.
    let Some(window) = web_sys::window() else {
        return;
    };
    let window_target: EventTarget = window.into();

    let event_closure: Closure<dyn FnMut(Event)> = {
        let f = reposition_fn.clone();
        Closure::wrap(Box::new(move |_: Event| {
            let _ = f.call0(&JsValue::UNDEFINED);
        }) as Box<dyn FnMut(Event)>)
    };
    let scroll_opts = AddEventListenerOptions::new();
    scroll_opts.set_capture(true);
    scroll_opts.set_passive(true);
    let _ = window_target.add_event_listener_with_callback_and_add_event_listener_options(
        "scroll",
        event_closure.as_ref().unchecked_ref(),
        &scroll_opts,
    );
    let _ = window_target
        .add_event_listener_with_callback("resize", event_closure.as_ref().unchecked_ref());

    // Stash everything so `release` can tear it all down later.
    let state = AnchorState {
        inner,
        window_target,
        anchor_observer,
        floater_observer,
        reposition_closure,
        obs_callback,
        event_closure,
    };

    let boxed: Box<AnchorState> = Box::new(state);
    let ptr = Box::into_raw(boxed) as usize as f64;
    let _ = Reflect::set(floater.as_ref(), &STATE_KEY.into(), &JsValue::from_f64(ptr));
}

/// Called by `walker::release_subtree`. Tears down the shared
/// closures + listeners.
pub fn release(el: &Element) {
    let v = match Reflect::get(el.as_ref(), &STATE_KEY.into()) {
        Ok(v) => v,
        Err(_) => return,
    };
    if v.is_undefined() || v.is_null() {
        return;
    }
    let Some(ptr_f) = v.as_f64() else { return };
    let ptr = ptr_f as usize as *mut AnchorState;
    if ptr.is_null() {
        return;
    }
    // Safety: this pointer was produced by `Box::into_raw` in `run`
    // and stashed under STATE_KEY. `release` is called at most once
    // per element (the walker evicts the private slot when the scope
    // tears down — there's no other reader).
    let state = unsafe { Box::from_raw(ptr) };

    // Remove listeners before dropping the closures that back them.
    // The `scroll` listener was installed with `capture: true`;
    // `removeEventListener` only matches if we pass the same
    // capture flag, otherwise the DOM keeps the listener attached
    // and the closure is invoked after its Rust backing memory
    // has been freed — "closure invoked recursively or after being
    // dropped" on every subsequent scroll.
    let target = state.window_target.clone();
    let _ = target.remove_event_listener_with_callback_and_bool(
        "scroll",
        state.event_closure.as_ref().unchecked_ref(),
        true,
    );
    let _ = target.remove_event_listener_with_callback(
        "resize",
        state.event_closure.as_ref().unchecked_ref(),
    );
    if let Some(obs) = &state.anchor_observer {
        obs.disconnect();
    }
    if let Some(obs) = &state.floater_observer {
        obs.disconnect();
    }
    let _ = Reflect::set(el.as_ref(), &STATE_KEY.into(), &JsValue::UNDEFINED);
    // `state` drops here, taking the closures with it.
    drop(state);
}

struct AnchorInner {
    anchor: Element,
    floater: HtmlElement,
    placement: Placement,
    offset: f64,
    flip: bool,
}

// Fields that are only read "through" the JS runtime — e.g. the
// reposition_closure is held solely to keep its JS function pointer
// valid, never inspected from Rust. Silence the dead-code lint at
// the struct level rather than per-field.
#[allow(dead_code)]
struct AnchorState {
    inner: Rc<RefCell<AnchorInner>>,
    window_target: EventTarget,
    anchor_observer: Option<ResizeObserver>,
    floater_observer: Option<ResizeObserver>,
    reposition_closure: Closure<dyn FnMut()>,
    obs_callback: Closure<dyn FnMut(JsValue, JsValue)>,
    event_closure: Closure<dyn FnMut(Event)>,
}

fn reposition(inner: &AnchorInner) {
    let Some(window) = web_sys::window() else { return };
    let vw = window.inner_width().ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
    let vh = window.inner_height().ok().and_then(|v| v.as_f64()).unwrap_or(0.0);

    let a = inner.anchor.get_bounding_client_rect();
    let f = inner.floater.get_bounding_client_rect();

    let mut side = inner.placement.side;
    if inner.flip {
        side = maybe_flip(side, &a, &f, vw, vh, inner.offset);
    }
    let eff = Placement {
        side,
        align: inner.placement.align,
    };
    let (x, y) = compute_xy(eff, &a, &f, inner.offset);

    let style = inner.floater.style();
    let _ = style.set_property("position", "fixed");
    let _ = style.set_property("top", &format!("{}px", y.round()));
    let _ = style.set_property("left", &format!("{}px", x.round()));
    let _ = style.set_property("right", "auto");
    let _ = style.set_property("bottom", "auto");
}

fn maybe_flip(
    side: Side,
    a: &web_sys::DomRect,
    f: &web_sys::DomRect,
    vw: f64,
    vh: f64,
    offset: f64,
) -> Side {
    let needed_main = match side {
        Side::Top | Side::Bottom => f.height() + offset,
        Side::Left | Side::Right => f.width() + offset,
    };
    let (room, opp_room) = match side {
        Side::Top => (a.top(), vh - a.bottom()),
        Side::Bottom => (vh - a.bottom(), a.top()),
        Side::Left => (a.left(), vw - a.right()),
        Side::Right => (vw - a.right(), a.left()),
    };
    if room < needed_main && opp_room > room {
        Placement {
            side,
            align: Align::Center,
        }
        .opposite()
        .side
    } else {
        side
    }
}

fn compute_xy(p: Placement, a: &web_sys::DomRect, f: &web_sys::DomRect, offset: f64) -> (f64, f64) {
    match p.side {
        Side::Top => {
            let y = a.top() - f.height() - offset;
            let x = cross_axis_x(p.align, a, f);
            (x, y)
        }
        Side::Bottom => {
            let y = a.bottom() + offset;
            let x = cross_axis_x(p.align, a, f);
            (x, y)
        }
        Side::Left => {
            let x = a.left() - f.width() - offset;
            let y = cross_axis_y(p.align, a, f);
            (x, y)
        }
        Side::Right => {
            let x = a.right() + offset;
            let y = cross_axis_y(p.align, a, f);
            (x, y)
        }
    }
}

fn cross_axis_x(align: Align, a: &web_sys::DomRect, f: &web_sys::DomRect) -> f64 {
    match align {
        Align::Start => a.left(),
        Align::Center => a.left() + (a.width() - f.width()) / 2.0,
        Align::End => a.right() - f.width(),
    }
}

fn cross_axis_y(align: Align, a: &web_sys::DomRect, f: &web_sys::DomRect) -> f64 {
    match align {
        Align::Start => a.top(),
        Align::Center => a.top() + (a.height() - f.height()) / 2.0,
        Align::End => a.bottom() - f.height(),
    }
}

fn parse_placement(raw: Option<&str>) -> Placement {
    let raw = raw.unwrap_or("bottom");
    let (side_s, align_s) = match raw.split_once('-') {
        Some((s, a)) => (s, Some(a)),
        None => (raw, None),
    };
    let side = match side_s {
        "top" => Side::Top,
        "bottom" => Side::Bottom,
        "left" => Side::Left,
        "right" => Side::Right,
        _ => Side::Bottom,
    };
    let align = match align_s {
        Some("start") => Align::Start,
        Some("end") => Align::End,
        _ => Align::Center,
    };
    Placement { side, align }
}

/// Parse `.offset.<N>` from the modifier list. Accepts negative
/// integers (`-4`). Returns `0.0` when absent or unparseable.
fn parse_offset(modifiers: &[String]) -> f64 {
    for (i, m) in modifiers.iter().enumerate() {
        if m == "offset" {
            if let Some(n) = modifiers.get(i + 1).and_then(|s| s.parse::<f64>().ok()) {
                return n;
            }
        }
    }
    0.0
}

/// Resolve the anchor. Order:
///
/// 1. `raw` as a `pp-ref` name on the current scope.
/// 2. `raw` as a CSS selector on the document.
/// 3. If `raw` is an identifier and resolves to a string scope
///    field, treat that string as selector (recursively through
///    steps 2). Lets Pine components pass a `anchor` prop and
///    reference it in the template as `pp-anchor="anchor"` without
///    needing the substrate to special-case reactive directive
///    values.
fn resolve_anchor(call: &DirectiveCall) -> Option<Element> {
    let raw = call.value.trim();
    if raw.is_empty() {
        return None;
    }
    if is_identifier(raw) {
        if let Some(el) = refs::get_on(call.scope_id, raw) {
            return Some(el);
        }
    }
    let doc = web_sys::window()?.document()?;
    if let Some(el) = doc.query_selector(raw).ok().flatten() {
        return Some(el);
    }
    // Scope-field fallback — resolve `raw` as a field on the
    // current scope's proxy; if it's a non-empty string, try it as
    // a ref / selector.
    if is_identifier(raw) {
        let v = Reflect::get(call.proxy, &JsValue::from_str(raw))
            .unwrap_or(JsValue::UNDEFINED);
        if let Some(s) = v.as_string() {
            let s = s.trim();
            if !s.is_empty() {
                if is_identifier(s) {
                    if let Some(el) = refs::get_on(call.scope_id, s) {
                        return Some(el);
                    }
                }
                return doc.query_selector(s).ok().flatten();
            }
        }
    }
    None
}

fn is_identifier(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    it.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_placement_default_is_bottom_center() {
        let p = parse_placement(None);
        assert!(matches!(p.side, Side::Bottom));
        assert!(matches!(p.align, Align::Center));
    }

    #[test]
    fn parse_placement_side_only() {
        let p = parse_placement(Some("top"));
        assert!(matches!(p.side, Side::Top));
        assert!(matches!(p.align, Align::Center));
    }

    #[test]
    fn parse_placement_side_plus_align() {
        let p = parse_placement(Some("bottom-end"));
        assert!(matches!(p.side, Side::Bottom));
        assert!(matches!(p.align, Align::End));
    }

    #[test]
    fn parse_placement_invalid_side_falls_back_to_bottom() {
        let p = parse_placement(Some("upwards"));
        assert!(matches!(p.side, Side::Bottom));
    }

    #[test]
    fn parse_placement_invalid_align_becomes_center() {
        let p = parse_placement(Some("top-sideways"));
        assert!(matches!(p.side, Side::Top));
        assert!(matches!(p.align, Align::Center));
    }

    #[test]
    fn parse_offset_missing_is_zero() {
        assert_eq!(parse_offset(&mods(&[])), 0.0);
    }

    #[test]
    fn parse_offset_positive_and_negative() {
        assert_eq!(parse_offset(&mods(&["offset", "12"])), 12.0);
        assert_eq!(parse_offset(&mods(&["offset", "-4"])), -4.0);
    }

    #[test]
    fn parse_offset_ignores_junk_value() {
        assert_eq!(parse_offset(&mods(&["offset", "nope"])), 0.0);
    }

    #[test]
    fn is_identifier_accepts_ref_names() {
        assert!(is_identifier("trigger"));
        assert!(is_identifier("my-button"));
        assert!(is_identifier("_hidden"));
    }

    #[test]
    fn is_identifier_rejects_selectors() {
        assert!(!is_identifier("#trigger"));
        assert!(!is_identifier(".btn"));
        assert!(!is_identifier("[data-anchor]"));
        assert!(!is_identifier("1bad"));
        assert!(!is_identifier(""));
    }

    #[test]
    fn opposite_flips_main_axis_only() {
        let p = Placement {
            side: Side::Top,
            align: Align::End,
        };
        let o = p.opposite();
        assert!(matches!(o.side, Side::Bottom));
        assert!(matches!(o.align, Align::End));
    }
}
