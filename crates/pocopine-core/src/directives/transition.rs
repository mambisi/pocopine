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
    /// When true, `enter` / `leave` fire `on_done` synchronously and
    /// skip the CSS-class machinery. Tests opt in via
    /// `pocopine::animate::disable()` so previously-instant
    /// mount/unmount assertions stay fast after RFC-038's defaults
    /// gave every Pine primitive a real CSS transition.
    static DISABLED: Cell<bool> = const { Cell::new(false) };
}

/// Globally turn off CSS transitions for `pp-transition`. Intended
/// for tests; production code should leave the default in place.
pub fn set_disabled(v: bool) {
    DISABLED.with(|c| c.set(v));
}

pub fn is_disabled() -> bool {
    DISABLED.with(|c| c.get())
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
///
/// Honours `prefers-reduced-motion` (RFC-039 §1) — when reduced and
/// the element has no `data-pp-motion="always"` opt-out, fires
/// `on_done` synchronously and skips the class swap. Authors who
/// want motion under reduced-motion stamp `data-pp-motion="always"`
/// (the `#[component(motion = "always")]` macro arg does this).
pub fn enter<F: FnOnce() + 'static>(el: &Element, on_done: F) {
    if is_disabled() {
        on_done();
        return;
    }
    if crate::animate::motion::effective_for(el) == crate::animate::motion::MotionPreference::Reduced
    {
        on_done();
        return;
    }
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

    // Two-phase swap. Apply ONLY `enter-start` (the initial state)
    // first, force a reflow so the browser commits opacity:0 /
    // transform:scale(...) WITHOUT a transition rule active. Then
    // on next frame, add `enter` (which carries the
    // `transition: opacity ..., transform ...` shorthand) AND
    // `enter-end` (the final state) at once, removing
    // `enter-start`. The browser sees the property change with a
    // transition rule already present and starts a clean tween.
    //
    // The earlier single-phase pattern (add base+from together)
    // looked correct but kicked off a TRANSITION from the prior
    // computed value (opacity:1 for a freshly cloned element) to
    // the from-state (opacity:0). The next-frame swap to `to` then
    // interrupted that in-flight tween with a new target near the
    // current value, leaving opacity stuck around 1 — the
    // user-visible "content settles before the overlay fades in"
    // flicker on Pine Dialog.
    let cl = el.class_list();
    {
        let s = rc.borrow();
        for c in &s.enter_start {
            let _ = cl.add_1(c);
        }
    }
    let _ = el.client_width();

    let el_cap = el.clone();
    let rc_cap = rc.clone();
    let on_done_cell = std::rc::Rc::new(std::cell::RefCell::new(Some(on_done)));
    crate::tick::next_frame(move || {
        if rc_cap.borrow().epoch != epoch {
            return;
        }
        let cl = el_cap.class_list();
        {
            let s = rc_cap.borrow();
            for c in &s.enter_start {
                let _ = cl.remove_1(c);
            }
            for c in &s.enter {
                let _ = cl.add_1(c);
            }
            for c in &s.enter_end {
                let _ = cl.add_1(c);
            }
        }
        let el_for_end = el_cap.clone();
        let rc_for_end = rc_cap.clone();
        let on_done_cell = on_done_cell.clone();
        schedule_end(&el_cap, rc_cap.clone(), epoch, move || {
            // KEEP `enter` (base) + `enter-end` (to) on the element
            // post-settle. Removing them at the moment the
            // transition completes triggered a visible end-of-enter
            // flicker: `transform: matrix(1,0,0,1,0,0)` snapping
            // to `transform: none` (semantically identical, but
            // browsers re-rasterize anti-aliased glyphs / borders
            // on style change). The `enter` class still has the
            // `transition: opacity ...` rule active, so a later
            // author-side style change to opacity / transform
            // would tween — but for Pine compounds those are
            // stable post-mount, and the trade-off (no flicker)
            // wins. `cancel()` at the start of `leave()` clears
            // these before the leave dispatch, so the next round
            // has a clean slate.
            let _ = el_for_end;
            let mut s = rc_for_end.borrow_mut();
            s.phase = Phase::Idle;
            drop(s);
            if let Some(cb) = on_done_cell.borrow_mut().take() {
                cb();
            }
        });
    });
}

/// Run the leave sequence on `el`, then invoke `on_done`. Callers pass
/// the real "hide or remove" work as `on_done` so the DOM mutation
/// happens after the animation completes. With no transition attrs,
/// `on_done` fires synchronously.
pub fn leave<F: FnOnce() + 'static>(el: &Element, on_done: F) {
    if is_disabled() {
        on_done();
        return;
    }
    if crate::animate::motion::effective_for(el) == crate::animate::motion::MotionPreference::Reduced
    {
        on_done();
        return;
    }
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

    // Mirror enter()'s two-phase swap. For leave: the element is
    // currently visible (opacity:1 etc.). Apply ONLY `leave-start`
    // first to lock in the current rendered state explicitly, then
    // on next frame add `leave` (transition rule) and `leave-end`
    // (target state) at once so the browser tweens cleanly from
    // start to end.
    let cl = el.class_list();
    {
        let s = rc.borrow();
        for c in &s.leave_start {
            let _ = cl.add_1(c);
        }
    }
    let _ = el.client_width();

    let el_cap = el.clone();
    let rc_cap = rc.clone();
    let on_done_cell = std::rc::Rc::new(std::cell::RefCell::new(Some(on_done)));
    crate::tick::next_frame(move || {
        if rc_cap.borrow().epoch != epoch {
            return;
        }
        let cl = el_cap.class_list();
        {
            let s = rc_cap.borrow();
            for c in &s.leave_start {
                let _ = cl.remove_1(c);
            }
            for c in &s.leave {
                let _ = cl.add_1(c);
            }
            for c in &s.leave_end {
                let _ = cl.add_1(c);
            }
        }
        let el_for_end = el_cap.clone();
        let rc_for_end = rc_cap.clone();
        let on_done_cell = on_done_cell.clone();
        schedule_end(&el_cap, rc_cap.clone(), epoch, move || {
            // Same reasoning as enter: keep the leave classes
            // applied. For pp-if leave, the on_done callback
            // removes the element entirely — the classes go with
            // it. For pp-show leave, the on_done sets `display:
            // none` so the leftover classes are invisible until
            // the next enter, which `cancel`s them in its first
            // step.
            let _ = el_for_end;
            let mut s = rc_for_end.borrow_mut();
            s.phase = Phase::Idle;
            drop(s);
            if let Some(cb) = on_done_cell.borrow_mut().take() {
                cb();
            }
        });
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
    // `transition-*` may be comma-separated when properties have
    // different timings (e.g. `opacity 100ms, transform 250ms`).
    // Take the MAX so `schedule_end` waits for the longest property
    // — RFC-039 §4 fixes a bug where reading the first value early-
    // fired `on_done` and yanked the element mid-transform.
    s.split(',')
        .map(|seg| {
            let t = seg.trim();
            if let Some(n) = t.strip_suffix("ms") {
                n.trim().parse::<f64>().unwrap_or(0.0)
            } else if let Some(n) = t.strip_suffix('s') {
                n.trim().parse::<f64>().unwrap_or(0.0) * 1000.0
            } else {
                0.0
            }
        })
        .fold(0.0_f64, f64::max)
}

/// Selector matching every element that carries any preset attr
/// (shorthand or six-attr form). Used by the subtree helpers to
/// gather descendants whose transitions should fire alongside the
/// pp-if/pp-show toggle on the clone root.
const ATTR_SELECTOR: &str = "[pp-transition], [pp-transition\\:in], [pp-transition\\:out], \
    [pp-transition\\:enter], [pp-transition\\:enter-start], [pp-transition\\:enter-end], \
    [pp-transition\\:leave], [pp-transition\\:leave-start], [pp-transition\\:leave-end]";

fn has_any_transition_attr(el: &Element) -> bool {
    el.has_attribute("pp-transition")
        || el.has_attribute("pp-transition:in")
        || el.has_attribute("pp-transition:out")
        || el.has_attribute("pp-transition:enter")
        || el.has_attribute("pp-transition:enter-start")
        || el.has_attribute("pp-transition:enter-end")
        || el.has_attribute("pp-transition:leave")
        || el.has_attribute("pp-transition:leave-start")
        || el.has_attribute("pp-transition:leave-end")
}

fn collect_animated(root: &Element) -> Vec<Element> {
    use wasm_bindgen::JsCast;
    let mut out = Vec::new();
    if has_any_transition_attr(root) {
        out.push(root.clone());
    }
    if let Ok(list) = root.query_selector_all(ATTR_SELECTOR) {
        for i in 0..list.length() {
            if let Some(node) = list.item(i) {
                if let Ok(el) = node.dyn_into::<Element>() {
                    out.push(el);
                }
            }
        }
    }
    out
}

/// Run the enter sequence on `root` AND every descendant carrying
/// any `pp-transition:*` attr. Compound primitives stamp preset
/// attrs on inner custom-element children (`<pine-dialog-content>`)
/// rather than the pp-if clone root (the portal `<div>`), so
/// callers walking the subtree pick those up. `on_done` fires after
/// the longest enter completes; with no animated elements it's
/// synchronous.
pub fn enter_subtree<F: FnOnce() + 'static>(root: &Element, on_done: F) {
    let elems = collect_animated(root);
    if elems.is_empty() {
        on_done();
        return;
    }
    let remaining = Rc::new(Cell::new(elems.len()));
    let on_done_cell = Rc::new(RefCell::new(Some(on_done)));
    for el in elems {
        let remaining = remaining.clone();
        let on_done_cell = on_done_cell.clone();
        enter(&el, move || {
            let n = remaining.get().saturating_sub(1);
            remaining.set(n);
            if n == 0 {
                if let Some(cb) = on_done_cell.borrow_mut().take() {
                    cb();
                }
            }
        });
    }
}

/// Like [`enter_subtree`] but each animated descendant fires with
/// an additional `i * stagger_ms` delay (where `i` is its index in
/// the collected list). Useful for sequenced reveals on `pp-for`
/// list mounts (RFC-039 §6).
///
/// `on_done` fires after the LAST animation settles. Pass
/// `stagger_ms = 0` for the same behaviour as [`enter_subtree`].
pub fn enter_subtree_staggered<F: FnOnce() + 'static>(
    root: &Element,
    stagger_ms: u32,
    on_done: F,
) {
    let elems = collect_animated(root);
    if elems.is_empty() {
        on_done();
        return;
    }
    let remaining = Rc::new(Cell::new(elems.len()));
    let on_done_cell = Rc::new(RefCell::new(Some(on_done)));
    for (i, el) in elems.into_iter().enumerate() {
        let remaining = remaining.clone();
        let on_done_cell = on_done_cell.clone();
        let delay = (i as u32).saturating_mul(stagger_ms);
        let fire = move || {
            enter(&el, move || {
                let n = remaining.get().saturating_sub(1);
                remaining.set(n);
                if n == 0 {
                    if let Some(cb) = on_done_cell.borrow_mut().take() {
                        cb();
                    }
                }
            });
        };
        if delay == 0 {
            fire();
        } else {
            let cb = wasm_bindgen::closure::Closure::once_into_js(fire);
            if let Some(w) = web_sys::window() {
                let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                    cb.unchecked_ref(),
                    delay as i32,
                );
            }
        }
    }
}

/// Mirror of [`enter_subtree`] for unmount. Dispatches `leave` to
/// every animated element in the subtree in parallel; the caller's
/// `on_done` (typically the actual DOM removal) fires once they all
/// complete. Synchronous when no element animates.
pub fn leave_subtree<F: FnOnce() + 'static>(root: &Element, on_done: F) {
    let elems = collect_animated(root);
    if elems.is_empty() {
        on_done();
        return;
    }
    let remaining = Rc::new(Cell::new(elems.len()));
    let on_done_cell = Rc::new(RefCell::new(Some(on_done)));
    for el in elems {
        let remaining = remaining.clone();
        let on_done_cell = on_done_cell.clone();
        leave(&el, move || {
            let n = remaining.get().saturating_sub(1);
            remaining.set(n);
            if n == 0 {
                if let Some(cb) = on_done_cell.borrow_mut().take() {
                    cb();
                }
            }
        });
    }
}

/// True if any element in the subtree (root included) is mid-leave.
pub fn is_subtree_leaving(root: &Element) -> bool {
    collect_animated(root).iter().any(is_leaving)
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
