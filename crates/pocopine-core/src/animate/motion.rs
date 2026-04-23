//! Motion-preference detection — RFC-039 §1.
//!
//! Reads the user's `(prefers-reduced-motion: reduce)` system
//! setting once at install and tracks `matchMedia` change events so
//! the runtime stays responsive when the user flips it. Consumers
//! call [`current`] for a snapshot or [`is_reduced`] for the boolean
//! shortcut.
//!
//! `transition::enter` / `leave` short-circuit on `is_reduced()` to
//! invoke their `on_done` callback synchronously. The CSS atom sheet
//! also collapses every preset's duration to 1ms under reduced
//! motion (so transitions still fire, just imperceptibly), which
//! keeps `transitionend` semantics intact for any author code that
//! relies on them.
//!
//! Per-element opt-out: stamp `data-pp-motion="always"` on an
//! element (or the root of a subtree) to keep the full motion
//! durations regardless of the system preference. The
//! `#[component(motion = "always")]` macro arg adds this stamp.

use std::cell::Cell;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

/// User motion preference snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionPreference {
    /// Default — motion plays at preset durations.
    Full,
    /// `(prefers-reduced-motion: reduce)` matches.
    Reduced,
}

thread_local! {
    static REDUCED: Cell<bool> = const { Cell::new(false) };
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// Read the matchMedia state and start listening for changes. Idempotent.
pub fn install() {
    INSTALLED.with(|installed| {
        if installed.get() {
            return;
        }
        installed.set(true);
    });
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(mql)) = window.match_media("(prefers-reduced-motion: reduce)") else {
        return;
    };
    REDUCED.with(|c| c.set(mql.matches()));
    // Listen for changes — user can flip the preference at any time.
    let listener = Closure::<dyn Fn(web_sys::Event)>::new(|ev: web_sys::Event| {
        let Some(target) = ev.target() else { return };
        let Ok(mql) = target.dyn_into::<web_sys::MediaQueryList>() else {
            return;
        };
        REDUCED.with(|c| c.set(mql.matches()));
    });
    let _ = mql.add_event_listener_with_callback("change", listener.as_ref().unchecked_ref());
    listener.forget();
}

/// Current motion preference snapshot. Cheap — reads a thread-local.
pub fn current() -> MotionPreference {
    if REDUCED.with(|c| c.get()) {
        MotionPreference::Reduced
    } else {
        MotionPreference::Full
    }
}

/// Convenience boolean — true when the user prefers reduced motion.
pub fn is_reduced() -> bool {
    REDUCED.with(|c| c.get())
}

/// Walk up `el`'s ancestor chain looking for a `data-pp-motion`
/// override. Returns `Some(true)` for `"always"` (force full
/// motion), `Some(false)` for `"reduce"`, or `None` for the
/// default (defer to system preference).
pub fn element_override(el: &web_sys::Element) -> Option<bool> {
    let mut cur: Option<web_sys::Element> = Some(el.clone());
    while let Some(e) = cur {
        if let Some(v) = e.get_attribute("data-pp-motion") {
            return match v.as_str() {
                "always" => Some(true),
                "reduce" => Some(false),
                _ => None,
            };
        }
        cur = e.parent_element();
    }
    None
}

/// Effective motion preference for `el` — combines element overrides
/// with the system preference.
pub fn effective_for(el: &web_sys::Element) -> MotionPreference {
    match element_override(el) {
        Some(true) => MotionPreference::Full,
        Some(false) => MotionPreference::Reduced,
        None => current(),
    }
}

#[cfg(test)]
mod tests {
    use super::MotionPreference;
    #[test]
    fn motion_preference_eq() {
        assert_eq!(MotionPreference::Full, MotionPreference::Full);
        assert_ne!(MotionPreference::Full, MotionPreference::Reduced);
    }
}
