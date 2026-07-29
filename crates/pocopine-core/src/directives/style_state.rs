//! Coordination for directives that share the inline `style` declaration.
//!
//! `:style` owns the bound declaration while `pp-show` overlays `display:
//! none`. Keeping the overlay separate prevents a reactive whole-attribute
//! style write from making a hidden element visible, and lets `pp-show`
//! restore the latest bound/static `display` value when the element is shown.

use js_sys::Reflect;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, HtmlElement};

const BASE_CAPTURED_KEY: &str = "__pp_style_base_display_captured";
const BASE_DISPLAY_KEY: &str = "__pp_style_base_display";
const BASE_PRIORITY_KEY: &str = "__pp_style_base_display_priority";
const SHOW_HIDDEN_KEY: &str = "__pp_show_hidden";

/// Apply the declaration owned by `:style`, then restore the `pp-show`
/// visibility overlay when the element is currently hidden.
pub(crate) fn apply_bound_style(el: &Element, declaration: Option<&str>) {
    match declaration {
        Some(declaration) => {
            let _ = el.set_attribute("style", declaration);
        }
        None => {
            let _ = el.remove_attribute("style");
        }
    }

    let Some(html_el) = el.dyn_ref::<HtmlElement>() else {
        return;
    };
    capture_base_display(html_el);
    if is_show_hidden(html_el) {
        hide(html_el);
    }
}

/// Apply `pp-show`'s visibility without losing an inline `display` declaration
/// owned by static markup or `:style`.
pub(crate) fn set_visible(el: &HtmlElement, visible: bool) {
    ensure_base_display(el);
    set_bool(el, SHOW_HIDDEN_KEY, !visible);
    if visible {
        restore_base_display(el);
    } else {
        hide(el);
    }
}

fn ensure_base_display(el: &HtmlElement) {
    if !get_bool(el, BASE_CAPTURED_KEY) {
        capture_base_display(el);
    }
}

fn capture_base_display(el: &HtmlElement) {
    let style = el.style();
    let display = style.get_property_value("display").unwrap_or_default();
    let priority = style.get_property_priority("display");
    set_string(el, BASE_DISPLAY_KEY, &display);
    set_string(el, BASE_PRIORITY_KEY, &priority);
    set_bool(el, BASE_CAPTURED_KEY, true);
}

fn restore_base_display(el: &HtmlElement) {
    let style = el.style();
    let display = get_string(el, BASE_DISPLAY_KEY);
    if display.is_empty() {
        let _ = style.remove_property("display");
    } else {
        let priority = get_string(el, BASE_PRIORITY_KEY);
        let _ = style.set_property_with_priority("display", &display, &priority);
    }
}

fn hide(el: &HtmlElement) {
    let _ = el.style().set_property("display", "none");
}

fn is_show_hidden(el: &HtmlElement) -> bool {
    get_bool(el, SHOW_HIDDEN_KEY)
}

fn get_bool(el: &HtmlElement, key: &str) -> bool {
    Reflect::get(el.as_ref(), &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn set_bool(el: &HtmlElement, key: &str, value: bool) {
    let _ = Reflect::set(
        el.as_ref(),
        &JsValue::from_str(key),
        &JsValue::from_bool(value),
    );
}

fn get_string(el: &HtmlElement, key: &str) -> String {
    Reflect::get(el.as_ref(), &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

fn set_string(el: &HtmlElement, key: &str, value: &str) {
    let _ = Reflect::set(
        el.as_ref(),
        &JsValue::from_str(key),
        &JsValue::from_str(value),
    );
}
