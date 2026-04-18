//! `pp-show="field"` — toggle display by truthiness of a scope field.

use wasm_bindgen::prelude::*;
use web_sys::HtmlElement;

use super::DirectiveCall;
use crate::path::resolve_path;
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
            let v = resolve_path(&proxy, &key);
            let truthy = !v.is_falsy();
            let style = html_el.style();
            if truthy {
                let _ = style.remove_property("display");
            } else {
                let _ = style.set_property("display", "none");
            }
        });
    });
    track_effect_on(call.el, id);
}
