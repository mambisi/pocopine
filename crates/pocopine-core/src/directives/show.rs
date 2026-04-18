//! `pp-show="field"` — toggle display by truthiness of a scope field.
//!
//! If the element also carries any `pp-transition:*` attributes, the
//! toggle goes through [`crate::directives::transition`] so the enter
//! / leave class sequence runs at the right moment; the `display:
//! none` is deferred until the leave animation completes.

use wasm_bindgen::prelude::*;
use web_sys::HtmlElement;

use super::transition;
use super::DirectiveCall;
use crate::path::resolve_truthy;
use crate::reactive::effect;
use crate::scope::with_current_el;
use crate::walker::track_effect_on;

pub fn run(call: &DirectiveCall) {
    // Gracefully degrade on non-HtmlElement (SVG etc.) — show is a no-op.
    let Ok(html_el): Result<HtmlElement, _> = call.el.clone().dyn_into() else { return };
    let proxy = call.proxy.clone();
    let key = call.value.clone();
    let el_for_track = call.el.clone();

    let id = effect(move || {
        with_current_el(&el_for_track.clone(), || {
            let truthy = resolve_truthy(&proxy, &key);
            let style = html_el.style();
            if truthy {
                // Unhide first so the enter animation plays against a
                // laid-out element, then run the class sequence.
                let _ = style.remove_property("display");
                transition::enter(html_el.as_ref(), || {});
            } else {
                let style_for_leave = style.clone();
                transition::leave(html_el.as_ref(), move || {
                    let _ = style_for_leave.set_property("display", "none");
                });
            }
        });
    });
    track_effect_on(call.el, id);
}
