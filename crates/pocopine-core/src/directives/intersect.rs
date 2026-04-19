//! `pp-intersect[:enter|:leave]="handler"` — RFC-016.
//!
//! Wraps `IntersectionObserver` so authors can run a handler when an
//! element crosses a visibility threshold. Handler receives one
//! `f64` arg: the `intersectionRatio`. Zero-arg handlers work too —
//! the `FromHandlerArg` pipeline drops extras.

use std::cell::Cell;
use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, IntersectionObserver, IntersectionObserverEntry, IntersectionObserverInit};

use super::DirectiveCall;
use crate::scope::invoke_handler;

const OBS_KEY: &str = "__pp_intersect_obs";

/// Which edge of the intersection transition the handler watches.
#[derive(Copy, Clone)]
enum Edge {
    Enter,
    Leave,
}

pub fn run(call: &DirectiveCall) {
    let handler = call.value.clone();
    let scope_id = call.scope_id;
    let el = call.el.clone();
    let once = call.modifiers.iter().any(|m| m == "once");

    let edge = match call.arg.as_deref() {
        Some("leave") => Edge::Leave,
        _ => Edge::Enter, // bare or `:enter`
    };

    let threshold = resolve_threshold(&call.modifiers);
    let root_margin = resolve_root_margin(&call.modifiers);

    let init = IntersectionObserverInit::new();
    init.set_threshold(&JsValue::from_f64(threshold));
    if let Some(m) = root_margin.as_deref() {
        init.set_root_margin(m);
    }

    // `Leave` requires the element to have been `Enter`ed at least
    // once — otherwise the synthetic initial-off-screen entry would
    // fire `:leave` immediately on subscribe, which is almost never
    // what the author wants.
    let seen_enter = Rc::new(Cell::new(false));
    // `fired_once` gates `.once`; set after the first *matching* fire.
    let fired_once = Rc::new(Cell::new(false));
    let obs_holder: Rc<Cell<Option<IntersectionObserver>>> = Rc::new(Cell::new(None));

    let cb_seen = seen_enter.clone();
    let cb_fired = fired_once.clone();
    let cb_holder = obs_holder.clone();
    let closure = Closure::wrap(Box::new(move |entries: JsValue, _obs: JsValue| {
        let Ok(entries) = entries.dyn_into::<Array>() else { return };
        for i in 0..entries.length() {
            let Ok(entry) = entries.get(i).dyn_into::<IntersectionObserverEntry>() else {
                continue;
            };
            let is_in = entry.is_intersecting();
            match edge {
                Edge::Enter => {
                    if !is_in {
                        continue;
                    }
                    cb_seen.set(true);
                }
                Edge::Leave => {
                    if is_in {
                        cb_seen.set(true);
                        continue;
                    }
                    if !cb_seen.get() {
                        // Never crossed the enter boundary yet; skip.
                        continue;
                    }
                }
            }

            if once && cb_fired.get() {
                continue;
            }

            let args = Array::new();
            args.push(&JsValue::from_f64(entry.intersection_ratio()));
            invoke_handler(scope_id, &handler, &args);

            if once {
                cb_fired.set(true);
                if let Some(obs) = cb_holder.take() {
                    obs.disconnect();
                }
            }
        }
    }) as Box<dyn FnMut(JsValue, JsValue)>);

    let Ok(observer) =
        IntersectionObserver::new_with_options(closure.as_ref().unchecked_ref(), &init)
    else {
        return;
    };
    observer.observe(&el);
    closure.forget();

    // Stash so `release` can disconnect, and so `.once` can reach
    // back in to disconnect from inside the callback.
    obs_holder.set(Some(observer.clone()));
    let _ = Reflect::set(el.as_ref(), &OBS_KEY.into(), observer.as_ref());
}

pub fn release(el: &Element) {
    let Ok(v) = Reflect::get(el.as_ref(), &OBS_KEY.into()) else { return };
    if v.is_undefined() || v.is_null() {
        return;
    }
    if let Ok(obs) = v.dyn_into::<IntersectionObserver>() {
        obs.disconnect();
    }
    let _ = Reflect::set(el.as_ref(), &OBS_KEY.into(), &JsValue::UNDEFINED);
}

/// Resolve the effective threshold from the modifier list. Explicit
/// `.threshold.N` wins over `.half` / `.full` shortcuts; missing
/// thresholds default to `0.0`. Numeric args are clamped to `[0, 1]`.
fn resolve_threshold(modifiers: &[String]) -> f64 {
    let mut explicit: Option<f64> = None;
    let mut i = 0;
    while i < modifiers.len() {
        if modifiers[i] == "threshold" {
            if let Some(n) = modifiers.get(i + 1).and_then(|s| s.parse::<f64>().ok()) {
                explicit = Some((n / 100.0).clamp(0.0, 1.0));
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    if let Some(n) = explicit {
        return n;
    }
    if modifiers.iter().any(|m| m == "full") {
        return 0.99;
    }
    if modifiers.iter().any(|m| m == "half") {
        return 0.5;
    }
    0.0
}

/// Resolve `rootMargin` from a `.margin.<v1>[.<v2>[.<v3>.<v4>]]`
/// modifier chain. Returns `None` when no `.margin` modifier exists
/// (so we can skip setting it and inherit the observer default).
fn resolve_root_margin(modifiers: &[String]) -> Option<String> {
    let idx = modifiers.iter().position(|m| m == "margin")?;
    let mut values: Vec<String> = Vec::with_capacity(4);
    for m in &modifiers[idx + 1..] {
        if values.len() == 4 {
            break;
        }
        if let Some(v) = parse_margin_value(m) {
            values.push(v);
        } else {
            break;
        }
    }
    if values.is_empty() {
        return None;
    }
    Some(values.join(" "))
}

/// A margin chunk is `Npx`, `N%`, or a bare `N` (treated as `Npx`).
/// Negative numbers allowed. Returns `None` for anything else.
fn parse_margin_value(raw: &str) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    if let Some(rest) = raw.strip_suffix("px") {
        rest.parse::<f64>().ok()?;
        return Some(raw.to_string());
    }
    if let Some(rest) = raw.strip_suffix('%') {
        rest.parse::<f64>().ok()?;
        return Some(raw.to_string());
    }
    if raw.parse::<f64>().is_ok() {
        return Some(format!("{raw}px"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn threshold_defaults_to_zero() {
        assert_eq!(resolve_threshold(&mods(&[])), 0.0);
    }

    #[test]
    fn threshold_half_and_full_shortcuts() {
        assert_eq!(resolve_threshold(&mods(&["half"])), 0.5);
        assert_eq!(resolve_threshold(&mods(&["full"])), 0.99);
    }

    #[test]
    fn threshold_numeric_overrides_shortcuts() {
        assert_eq!(resolve_threshold(&mods(&["half", "threshold", "25"])), 0.25);
        assert_eq!(resolve_threshold(&mods(&["threshold", "50", "full"])), 0.5);
    }

    #[test]
    fn threshold_clamps_out_of_range() {
        assert_eq!(resolve_threshold(&mods(&["threshold", "200"])), 1.0);
        assert_eq!(resolve_threshold(&mods(&["threshold", "-5"])), 0.0);
    }

    #[test]
    fn margin_missing_returns_none() {
        assert_eq!(resolve_root_margin(&mods(&["once"])), None);
    }

    #[test]
    fn margin_single_value_applies_to_all_sides() {
        assert_eq!(
            resolve_root_margin(&mods(&["margin", "200px"])).as_deref(),
            Some("200px")
        );
    }

    #[test]
    fn margin_bare_number_becomes_px() {
        assert_eq!(
            resolve_root_margin(&mods(&["margin", "200"])).as_deref(),
            Some("200px")
        );
    }

    #[test]
    fn margin_two_values() {
        assert_eq!(
            resolve_root_margin(&mods(&["margin", "10%", "25px"])).as_deref(),
            Some("10% 25px")
        );
    }

    #[test]
    fn margin_four_values() {
        assert_eq!(
            resolve_root_margin(&mods(&["margin", "10%", "25px", "25", "25px"])).as_deref(),
            Some("10% 25px 25px 25px")
        );
    }

    #[test]
    fn margin_allows_negative() {
        assert_eq!(
            resolve_root_margin(&mods(&["margin", "-100px"])).as_deref(),
            Some("-100px")
        );
    }

    #[test]
    fn margin_stops_at_non_value_token() {
        // `once` after `margin` terminates parsing — the sides set
        // before it is what we keep.
        assert_eq!(
            resolve_root_margin(&mods(&["margin", "50px", "once"])).as_deref(),
            Some("50px")
        );
    }
}
