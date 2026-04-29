//! `pp-flip` — layout animation on any element (RFC-039 §7).
//!
//! Generalises pp-for's keyed FLIP to arbitrary layout shifts.
//! Mark an element with `pp-flip` and its position will animate
//! from its old spot to its new one whenever the surrounding DOM
//! mutates and pushes it elsewhere.
//!
//! ```html
//! <ul pp-flip-container>
//!   <li pp-flip pp-for="item in items" pp-key="item">{item}</li>
//! </ul>
//! ```
//!
//! How it works:
//!
//! 1. Each `pp-flip` element registers itself in a thread-local
//!    map at directive-bind time and stamps its current
//!    `getBoundingClientRect` on the element via a private JS slot.
//! 2. A singleton MutationObserver on `document.body` watches every
//!    DOM mutation. When one fires, the directive schedules a
//!    next-frame check.
//! 3. On the next frame, every registered element has its current
//!    rect compared to its stored rect. Elements that moved beyond
//!    `min_delta_px` get `flip_from_snapshot`'d. The stored rect
//!    is then refreshed.
//!
//! Limitations:
//!
//! - Works for layout shifts caused by DOM mutations. Layout
//!   changes triggered by font load, scrollbar appearance, or
//!   container resize need a paired `pp-resize` until a future
//!   ResizeObserver-driven path lands.
//! - Honours `prefers-reduced-motion` via [`crate::animate::motion`].
//! - Cleaned up automatically on element removal — the
//!   MutationObserver also fires `removedNodes` and the entry is
//!   dropped from the registry.

use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use web_sys::{Element, MutationObserver, MutationObserverInit};

use crate::mount::track_effect_on;
use crate::reactive::{EffectId, ScopeId};

const FLIP_ID_KEY: &str = "__pp_flip_id";

struct Entry {
    el: Element,
    last_rect: web_sys::DomRect,
}

thread_local! {
    static REGISTRY: RefCell<HashMap<u64, Entry>> = RefCell::new(HashMap::new());
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(1) };
    static OBSERVER_INSTALLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SCHEDULED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Compiled-path install entry. Called by the compiled plan installer for
/// each `pp-flip` site the macro lifted into the template plan.
pub fn install_opaque(
    el: &Element,
    _arg: Option<&str>,
    _modifiers: &[&str],
    _value: &str,
    _scope_id: ScopeId,
    _proxy: &JsValue,
) {
    install_observer();
    let rect = el.get_bounding_client_rect();
    let id = NEXT_ID.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    REGISTRY.with(|m| {
        m.borrow_mut().insert(
            id,
            Entry {
                el: el.clone(),
                last_rect: rect,
            },
        );
    });
    let _ = js_sys::Reflect::set(
        el.as_ref(),
        &JsValue::from_str(FLIP_ID_KEY),
        &JsValue::from_f64(id as f64),
    );
    // No reactive dependency — the directive's behaviour is driven
    // by DOM mutations, not by a reactive expression. Track a
    // disposed effect so release_subtree picks up the unregister
    // hook below.
    let id_for_release = id;
    let release_id: EffectId = crate::reactive::effect(move || {
        let _ = id_for_release;
    });
    track_effect_on(el, release_id);
}

fn install_observer() {
    if OBSERVER_INSTALLED.with(|c| c.get()) {
        return;
    }
    OBSERVER_INSTALLED.with(|c| c.set(true));
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(body) = document.body() else { return };

    let cb = Closure::<dyn Fn(js_sys::Array, MutationObserver)>::new(
        |_records: js_sys::Array, _obs: MutationObserver| {
            schedule_check();
        },
    );
    if let Ok(observer) = MutationObserver::new(cb.as_ref().unchecked_ref()) {
        let init = MutationObserverInit::new();
        init.set_child_list(true);
        init.set_subtree(true);
        let _ = observer.observe_with_options(body.as_ref(), &init);
    }
    cb.forget();
}

/// Coalesce many mutations into a single next-frame check.
fn schedule_check() {
    if SCHEDULED.with(|c| c.get()) {
        return;
    }
    SCHEDULED.with(|c| c.set(true));
    crate::tick::next_frame(|| {
        SCHEDULED.with(|c| c.set(false));
        run_check();
    });
}

fn run_check() {
    // Snapshot keys to avoid borrow conflicts while mutating.
    let ids: Vec<u64> = REGISTRY.with(|m| m.borrow().keys().copied().collect());
    let mut to_drop: Vec<u64> = Vec::new();
    for id in ids {
        // Read the entry, then release the borrow before calling
        // `flip_from_snapshot` (which can re-enter via observers).
        let (el, old_rect) = match REGISTRY.with(|m| {
            m.borrow()
                .get(&id)
                .map(|e| (e.el.clone(), e.last_rect.clone()))
        }) {
            Some(p) => p,
            None => continue,
        };
        if !el.is_connected() {
            to_drop.push(id);
            continue;
        }
        let new_rect = el.get_bounding_client_rect();
        let dx = old_rect.left() - new_rect.left();
        let dy = old_rect.top() - new_rect.top();
        if dx.abs() >= 2.0 || dy.abs() >= 2.0 {
            crate::animate::flip_from_snapshot(
                &el,
                old_rect,
                crate::animate::FlipOptions::default(),
            );
        }
        // Refresh the stored rect for the next round.
        REGISTRY.with(|m| {
            if let Some(e) = m.borrow_mut().get_mut(&id) {
                e.last_rect = new_rect;
            }
        });
    }
    if !to_drop.is_empty() {
        REGISTRY.with(|m| {
            let mut map = m.borrow_mut();
            for id in to_drop {
                map.remove(&id);
            }
        });
    }
}

/// Release any pp-flip registry entry attached to `el`. Called
/// from the mount's `release_subtree` so disconnected elements
/// don't leak.
pub fn release(el: &Element) {
    let Some(v) = js_sys::Reflect::get(el.as_ref(), &JsValue::from_str(FLIP_ID_KEY))
        .ok()
        .and_then(|v| v.as_f64())
    else {
        return;
    };
    let id = v as u64;
    REGISTRY.with(|m| {
        m.borrow_mut().remove(&id);
    });
}
