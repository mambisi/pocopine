//! `pp-transition:*` — CSS-class based enter / leave animations.
//!
//! Per RFC-005. Six optional attributes provide the class strings:
//!
//! | Attribute | Phase |
//! |---|---|
//! | `pp-transition:enter`       | held for the whole enter phase |
//! | `pp-transition:enter-start` | one frame at the start of enter |
//! | `pp-transition:enter-end`   | rest of enter phase (replaces -start) |
//! | `pp-transition:leave`       | held for the whole leave phase |
//! | `pp-transition:leave-start` | one frame at the start of leave |
//! | `pp-transition:leave-end`   | rest of leave phase (replaces -start) |
//!
//! Callers don't register this as a directive. `pp-show` and `pp-if`
//! call [`enter`] / [`leave`] at mount/unmount time; those functions
//! lazy-init the per-element state by reading the attributes on first
//! use. When no transition attributes are present, the callbacks fire
//! synchronously — the "no-animation" path stays free.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::Reflect;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{window, Element};

const TX_ID_KEY: &str = "__pp_tx_id";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Entering,
    Leaving,
}

struct State {
    enter: Vec<String>,
    enter_start: Vec<String>,
    enter_end: Vec<String>,
    leave: Vec<String>,
    leave_start: Vec<String>,
    leave_end: Vec<String>,
    /// Incremented on every cancel / phase-start. End callbacks
    /// capture the epoch at schedule time and no-op if it moved.
    epoch: u64,
    phase: Phase,
    pending_timer: Option<i32>,
}

impl State {
    fn any(&self) -> bool {
        !self.enter.is_empty()
            || !self.enter_start.is_empty()
            || !self.enter_end.is_empty()
            || !self.leave.is_empty()
            || !self.leave_start.is_empty()
            || !self.leave_end.is_empty()
    }

    fn all_classes(&self) -> impl Iterator<Item = &String> {
        self.enter
            .iter()
            .chain(self.enter_start.iter())
            .chain(self.enter_end.iter())
            .chain(self.leave.iter())
            .chain(self.leave_start.iter())
            .chain(self.leave_end.iter())
    }
}

thread_local! {
    static TX: RefCell<HashMap<u64, Rc<RefCell<State>>>> =
        RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

fn get_or_init(el: &Element) -> Option<Rc<RefCell<State>>> {
    if let Some(v) = Reflect::get(el.as_ref(), &TX_ID_KEY.into())
        .ok()
        .and_then(|v| v.as_f64())
    {
        let id = v as u64;
        return TX.with(|m| m.borrow().get(&id).cloned());
    }
    // RFC-038 preset shorthand — if the element carries
    // `pp-transition="fade"` (symmetric) or the asymmetric split
    // `pp-transition:in="scale"` / `pp-transition:out="fade"`,
    // expand to the six `pp-transition:*` attrs here before
    // parsing. `apply_preset` is a no-op when the element already
    // has the six attrs (or when the name is `none` / unknown).
    expand_preset_shorthand(el);
    let state = parse_attrs(el);
    if !state.any() {
        return None;
    }
    let id = NEXT_ID.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    let rc = Rc::new(RefCell::new(state));
    TX.with(|m| m.borrow_mut().insert(id, rc.clone()));
    let _ = Reflect::set(
        el.as_ref(),
        &TX_ID_KEY.into(),
        &JsValue::from_f64(id as f64),
    );
    Some(rc)
}

/// If the element carries a preset shorthand (`pp-transition="fade"`
/// or `pp-transition:in` / `pp-transition:out`), expand it into the
/// six `pp-transition:*` class attrs via `animate::apply_preset`.
/// The already-six-attr author path stays untouched — explicit attrs
/// win if both are present (we only fill in attrs the author didn't
/// already set).
fn expand_preset_shorthand(el: &Element) {
    let sym = el.get_attribute("pp-transition");
    let in_attr = el.get_attribute("pp-transition:in");
    let out_attr = el.get_attribute("pp-transition:out");
    if sym.is_none() && in_attr.is_none() && out_attr.is_none() {
        return;
    }
    let symmetric = sym.as_deref().unwrap_or("");
    let in_name = in_attr.as_deref().unwrap_or(symmetric);
    let out_name = out_attr.as_deref().unwrap_or(symmetric);
    // Only stamp attrs that aren't already explicitly set by the
    // author — mix-and-match: author can override a single phase.
    let has = |name: &str| el.has_attribute(name);
    if has("pp-transition:enter")
        && has("pp-transition:enter-start")
        && has("pp-transition:enter-end")
        && has("pp-transition:leave")
        && has("pp-transition:leave-start")
        && has("pp-transition:leave-end")
    {
        return;
    }
    crate::animate::apply_preset(el, in_name, out_name);
}

fn parse_attrs(el: &Element) -> State {
    fn split(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }
    let get = |name: &str| {
        el.get_attribute(name)
            .map(|s| split(&s))
            .unwrap_or_default()
    };
    State {
        enter: get("pp-transition:enter"),
        enter_start: get("pp-transition:enter-start"),
        enter_end: get("pp-transition:enter-end"),
        leave: get("pp-transition:leave"),
        leave_start: get("pp-transition:leave-start"),
        leave_end: get("pp-transition:leave-end"),
        epoch: 0,
        phase: Phase::Idle,
        pending_timer: None,
    }
}

fn cancel(state: &mut State, el: &Element) {
    let cl = el.class_list();
    for c in state.all_classes() {
        let _ = cl.remove_1(c);
    }
    if let Some(h) = state.pending_timer.take() {
        if let Some(w) = window() {
            w.clear_timeout_with_handle(h);
        }
    }
    state.epoch = state.epoch.wrapping_add(1);
    state.phase = Phase::Idle;
}

/// True if `el` has a transition mid-leave. Consumers (`pp-if`) use
/// this to decide whether a flip-back-to-truthy needs to cancel the
/// pending unmount or can no-op.
pub fn is_leaving(el: &Element) -> bool {
    match get_or_init(el) {
        Some(rc) => rc.borrow().phase == Phase::Leaving,
        None => false,
    }
}

/// Run the enter sequence on `el`, then invoke `on_done`. If no
/// `pp-transition:*` attrs are present, invokes `on_done` synchronously.
pub fn enter<F: FnOnce() + 'static>(el: &Element, on_done: F) {
    let rc = match get_or_init(el) {
        Some(r) => r,
        None => {
            on_done();
            return;
        }
    };

    // Cancel whatever was in flight (leaving, or stale entering).
    {
        let mut s = rc.borrow_mut();
        cancel(&mut s, el);
        s.phase = Phase::Entering;
    }
    let epoch = rc.borrow().epoch;

    // Apply `enter` + `enter-start`, then force a reflow so the
    // browser commits the start state before we swap to end.
    let cl = el.class_list();
    {
        let s = rc.borrow();
        for c in s.enter.iter().chain(s.enter_start.iter()) {
            let _ = cl.add_1(c);
        }
    }
    let _ = el.client_width();

    {
        let s = rc.borrow();
        for c in &s.enter_start {
            let _ = cl.remove_1(c);
        }
        for c in &s.enter_end {
            let _ = cl.add_1(c);
        }
    }

    let el_cap = el.clone();
    let rc_cap = rc.clone();
    schedule_end(el, rc.clone(), epoch, move || {
        let cl = el_cap.class_list();
        let mut s = rc_cap.borrow_mut();
        for c in s.enter.iter().chain(s.enter_end.iter()) {
            let _ = cl.remove_1(c);
        }
        s.phase = Phase::Idle;
        drop(s);
        on_done();
    });
}

/// Run the leave sequence on `el`, then invoke `on_done`. Callers pass
/// the real "hide or remove" work as `on_done` so the DOM mutation
/// happens after the animation completes. With no transition attrs,
/// `on_done` fires synchronously.
pub fn leave<F: FnOnce() + 'static>(el: &Element, on_done: F) {
    let rc = match get_or_init(el) {
        Some(r) => r,
        None => {
            on_done();
            return;
        }
    };

    {
        let mut s = rc.borrow_mut();
        cancel(&mut s, el);
        s.phase = Phase::Leaving;
    }
    let epoch = rc.borrow().epoch;

    let cl = el.class_list();
    {
        let s = rc.borrow();
        for c in s.leave.iter().chain(s.leave_start.iter()) {
            let _ = cl.add_1(c);
        }
    }
    let _ = el.client_width();

    {
        let s = rc.borrow();
        for c in &s.leave_start {
            let _ = cl.remove_1(c);
        }
        for c in &s.leave_end {
            let _ = cl.add_1(c);
        }
    }

    let el_cap = el.clone();
    let rc_cap = rc.clone();
    schedule_end(el, rc.clone(), epoch, move || {
        let cl = el_cap.class_list();
        let mut s = rc_cap.borrow_mut();
        for c in s.leave.iter().chain(s.leave_end.iter()) {
            let _ = cl.remove_1(c);
        }
        s.phase = Phase::Idle;
        drop(s);
        on_done();
    });
}

/// Schedule `on_done` to fire after the element's computed
/// `transition-duration + transition-delay` (plus a small slop). If
/// the total is zero, fires synchronously — matches the "no transition
/// configured" fast path for pp-show/pp-if.
fn schedule_end<F: FnOnce() + 'static>(
    el: &Element,
    rc: Rc<RefCell<State>>,
    epoch: u64,
    on_done: F,
) {
    let duration = computed_duration_ms(el);
    if duration <= 0.0 {
        on_done();
        return;
    }
    let rc_cap = rc.clone();
    let closure = Closure::once(Box::new(move || {
        let current_epoch = rc_cap.borrow().epoch;
        if current_epoch != epoch {
            return;
        }
        rc_cap.borrow_mut().pending_timer = None;
        on_done();
    }) as Box<dyn FnOnce()>);
    if let Some(w) = window() {
        if let Ok(handle) = w.set_timeout_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            (duration + 20.0) as i32,
        ) {
            rc.borrow_mut().pending_timer = Some(handle);
        }
    }
    closure.forget();
}

fn computed_duration_ms(el: &Element) -> f64 {
    let Some(w) = window() else { return 0.0 };
    let Ok(Some(cs)) = w.get_computed_style(el) else {
        return 0.0;
    };
    let dur = cs
        .get_property_value("transition-duration")
        .unwrap_or_default();
    let delay = cs
        .get_property_value("transition-delay")
        .unwrap_or_default();
    parse_duration(&dur) + parse_duration(&delay)
}

fn parse_duration(s: &str) -> f64 {
    // `transition-*` can be comma-separated for multiple properties;
    // take the first value — it's the upper bound for any single
    // property anyway for the typical `transition: all ...ms` case.
    let first = s.split(',').next().unwrap_or("").trim();
    if let Some(n) = first.strip_suffix("ms") {
        n.trim().parse::<f64>().unwrap_or(0.0)
    } else if let Some(n) = first.strip_suffix('s') {
        n.trim().parse::<f64>().unwrap_or(0.0) * 1000.0
    } else {
        0.0
    }
}

/// Drop any transition state associated with `el`. Called from the
/// walker's `release_subtree` when an element is unmounted — keeps
/// the thread-local map from growing unboundedly.
pub fn release(el: &Element) {
    let Some(id) = Reflect::get(el.as_ref(), &TX_ID_KEY.into())
        .ok()
        .and_then(|v| v.as_f64())
    else {
        return;
    };
    let id = id as u64;
    TX.with(|m| {
        if let Some(rc) = m.borrow_mut().remove(&id) {
            if let Some(h) = rc.borrow_mut().pending_timer.take() {
                if let Some(w) = window() {
                    w.clear_timeout_with_handle(h);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn parses_ms() {
        assert_eq!(parse_duration("300ms"), 300.0);
    }

    #[test]
    fn parses_fractional_seconds() {
        assert_eq!(parse_duration("0.3s"), 300.0);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(parse_duration(""), 0.0);
    }

    #[test]
    fn takes_first_of_list() {
        assert_eq!(parse_duration("300ms, 200ms"), 300.0);
    }
}
