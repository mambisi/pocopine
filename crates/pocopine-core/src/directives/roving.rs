//! `pp-roving` — roving tabindex + arrow navigation (RFC-022).
//!
//! Container directive that wires one-tabstop-per-group keyboard
//! navigation. First enabled item gets `tabindex="0"`; rest get
//! `tabindex="-1"`. Arrow keys (per orientation), Home, and End
//! move focus and bump the tabindex with it.
//!
//! Two optional variants:
//!
//! - `pp-roving:<listbox-id>` on an `<input>` — the Command
//!   Palette **entry-transfer** form. First arrow keypress moves
//!   focus into the referenced listbox's first enabled item.
//! - `pp-roving:<listbox-id>.virtual` on any host (typically an
//!   editable `<input>`) — **activedescendant** form (RFC-034).
//!   DOM focus stays on the host; arrow keys move a virtual
//!   highlight through the listbox's items, communicated via
//!   the host's `aria-activedescendant` and each item's
//!   `data-highlighted` attribute.

use js_sys::Reflect;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{console, Element, HtmlElement, KeyboardEvent, NodeList};

use crate::reactive::ScopeId;

const STATE_KEY: &str = "__pp_roving_state";

#[derive(Copy, Clone, PartialEq, Eq)]
enum Orientation {
    Vertical,
    Horizontal,
    Both,
}

/// Compiled-path install entry. Called by `apply_static_plan` for
/// each `pp-roving[:<listbox-id>][.<modifier>...]` site the macro
/// lifted into the template plan.
pub fn install_opaque(
    el: &Element,
    arg: Option<&str>,
    modifiers: &[&str],
    _value: &str,
    _scope_id: ScopeId,
    _proxy: &JsValue,
) {
    let container = el.clone();
    let orientation = parse_orientation(modifiers);
    let wrap = !modifiers.contains(&"nowrap");
    let virtual_mode = modifiers
        .iter()
        .any(|m| *m == "virtual" || *m == "activedescendant");
    let linked_listbox = arg.map(|s| s.to_string());

    if virtual_mode {
        if let Some(listbox_id) = linked_listbox {
            let items_selector = parse_items_selector_virtual(modifiers);
            install_virtual(&container, listbox_id, orientation, wrap, items_selector);
        } else {
            console::warn_1(&JsValue::from_str(
                "pp-roving.virtual requires a :<listbox-id> argument",
            ));
        }
    } else if linked_listbox.is_some() {
        install_palette_entry(&container, orientation);
    } else {
        let items_selector = parse_items_selector(modifiers);
        install_roving(&container, orientation, wrap, items_selector);
    }
}

/// Called by `mount::release_subtree` on every released element.
pub fn release(el: &Element) {
    let Ok(v) = Reflect::get(el.as_ref(), &STATE_KEY.into()) else {
        return;
    };
    if v.is_undefined() || v.is_null() {
        return;
    }
    if let Ok(target) = v.clone().dyn_into::<web_sys::EventTarget>() {
        // Impossible to re-use the closure reference after it was
        // forgotten, so we rely on the `__pp_roving_state` slot
        // holding a handle the GC will drop alongside the element.
        let _ = target;
    }
    let _ = Reflect::set(el.as_ref(), &STATE_KEY.into(), &JsValue::UNDEFINED);
}

/// Primary roving install — container owns the arrow-key listener,
/// items get tabindex management.
fn install_roving(
    container: &Element,
    orientation: Orientation,
    wrap: bool,
    items_selector: String,
) {
    // Initial tabindex setup: everything → -1, first enabled → 0.
    // Skip this if any item already has `tabindex="0"` (author
    // explicitly picked an initial focus target).
    let items = query_items(container, &items_selector);
    let mut any_zero = false;
    for item in &items {
        if item.get_attribute("tabindex").as_deref() == Some("0") {
            any_zero = true;
            break;
        }
    }
    if !any_zero {
        for item in &items {
            let _ = item.set_attribute("tabindex", "-1");
        }
        if let Some(first) = items.iter().find(|i| !is_item_disabled(i)) {
            let _ = first.set_attribute("tabindex", "0");
        }
    }

    // Keydown listener.
    let container_clone = container.clone();
    let selector_clone = items_selector.clone();
    let closure: Closure<dyn FnMut(KeyboardEvent)> = Closure::wrap(Box::new({
        move |ev: KeyboardEvent| {
            let key = ev.key();
            let action = classify_key(&key, orientation);
            let Some(action) = action else { return };

            let items = query_items(&container_clone, &selector_clone);
            let enabled: Vec<Element> =
                items.into_iter().filter(|i| !is_item_disabled(i)).collect();
            if enabled.is_empty() {
                return;
            }

            let active = active_element();
            let cur_idx = active.as_ref().and_then(|a| {
                enabled.iter().position(|e| {
                    let a_el: &Element = a.as_ref();
                    e == a_el
                })
            });

            let last = enabled.len() - 1;
            let target_idx = match action {
                KeyAction::Next => match cur_idx {
                    Some(i) if i < last => i + 1,
                    Some(_) => {
                        if wrap {
                            0
                        } else {
                            last
                        }
                    }
                    None => 0,
                },
                KeyAction::Prev => match cur_idx {
                    Some(0) => {
                        if wrap {
                            last
                        } else {
                            0
                        }
                    }
                    Some(i) => i - 1,
                    None => last,
                },
                KeyAction::First => 0,
                KeyAction::Last => last,
            };

            ev.prevent_default();
            for (i, item) in enabled.iter().enumerate() {
                let _ = item.set_attribute("tabindex", if i == target_idx { "0" } else { "-1" });
            }
            if let Ok(html) = enabled[target_idx].clone().dyn_into::<HtmlElement>() {
                crate::focus::focus_no_scroll(&html);
            }
        }
    })
        as Box<dyn FnMut(KeyboardEvent)>);

    let target: &web_sys::EventTarget = container.as_ref();
    let _ = target.add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref());
    closure.forget();
    let _ = Reflect::set(container.as_ref(), &STATE_KEY.into(), &JsValue::TRUE);
}

/// Entry-transfer form: `pp-roving:<listbox-id>` on an `<input>`.
/// First arrow press moves focus into the listbox at the first (or
/// last, depending on direction) enabled item.
fn install_palette_entry(input: &Element, orientation: Orientation) {
    let input_clone = input.clone();
    let listbox_id = input.get_attribute("pp-roving").or_else(|| {
        // `call.arg` was parsed out of the attribute name earlier;
        // we get it from the container here to keep the closure
        // 'static-lifetime safe.
        Some(String::new())
    });
    // Actually capture the raw arg by reading the directive attribute
    // once and leaking the selector into the closure — call.arg is
    // consumed in `run`. We re-read here for locality.
    let listbox_arg = read_roving_arg(input);
    let _ = listbox_id;

    let closure: Closure<dyn FnMut(KeyboardEvent)> = Closure::wrap(Box::new({
        move |ev: KeyboardEvent| {
            let direction = match classify_key(&ev.key(), orientation) {
                Some(KeyAction::Next) => FocusEdge::First,
                Some(KeyAction::Prev) => FocusEdge::Last,
                _ => return,
            };
            let Some(id) = listbox_arg.as_deref() else {
                return;
            };
            let Some(listbox) = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(id))
            else {
                console::warn_1(&JsValue::from_str(&format!(
                    "pp-roving: listbox #{id} not found"
                )));
                return;
            };
            let items = query_items(&listbox, &default_items_selector());
            let enabled: Vec<Element> =
                items.into_iter().filter(|i| !is_item_disabled(i)).collect();
            if enabled.is_empty() {
                return;
            }
            let target = match direction {
                FocusEdge::First => enabled.first(),
                FocusEdge::Last => enabled.last(),
            };
            if let Some(t) = target {
                ev.prevent_default();
                // Make sure the listbox's own roving knows the new
                // position — move its tabindex onto the landing item.
                for (i, item) in enabled.iter().enumerate() {
                    let idx_match = match direction {
                        FocusEdge::First => i == 0,
                        FocusEdge::Last => i == enabled.len() - 1,
                    };
                    let _ = item.set_attribute("tabindex", if idx_match { "0" } else { "-1" });
                }
                if let Ok(html) = t.clone().dyn_into::<HtmlElement>() {
                    crate::focus::focus_no_scroll(&html);
                }
            }
        }
    })
        as Box<dyn FnMut(KeyboardEvent)>);
    let target: &web_sys::EventTarget = input_clone.as_ref();
    let _ = target.add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref());
    closure.forget();
    let _ = Reflect::set(input.as_ref(), &STATE_KEY.into(), &JsValue::TRUE);
}

enum FocusEdge {
    First,
    Last,
}

#[derive(Copy, Clone)]
enum KeyAction {
    Next,
    Prev,
    First,
    Last,
}

fn classify_key(key: &str, orientation: Orientation) -> Option<KeyAction> {
    let (next_keys, prev_keys): (&[&str], &[&str]) = match orientation {
        Orientation::Vertical => (&["ArrowDown"], &["ArrowUp"]),
        Orientation::Horizontal => (&["ArrowRight"], &["ArrowLeft"]),
        Orientation::Both => (&["ArrowDown", "ArrowRight"], &["ArrowUp", "ArrowLeft"]),
    };
    if next_keys.contains(&key) {
        return Some(KeyAction::Next);
    }
    if prev_keys.contains(&key) {
        return Some(KeyAction::Prev);
    }
    match key {
        "Home" => Some(KeyAction::First),
        "End" => Some(KeyAction::Last),
        _ => None,
    }
}

fn parse_orientation(modifiers: &[impl AsRef<str>]) -> Orientation {
    if modifiers.iter().any(|m| m.as_ref() == "horizontal") {
        return Orientation::Horizontal;
    }
    if modifiers.iter().any(|m| m.as_ref() == "both") {
        return Orientation::Both;
    }
    Orientation::Vertical
}

/// `items.<selector>` in the modifier chain overrides the default
/// selector. Missing → default.
fn parse_items_selector(modifiers: &[&str]) -> String {
    for (i, m) in modifiers.iter().enumerate() {
        if *m == "items" {
            if let Some(s) = modifiers.get(i + 1) {
                return (*s).to_string();
            }
        }
    }
    default_items_selector()
}

fn default_items_selector() -> String {
    concat!(
        "[role=\"menuitem\"], [role=\"menuitemradio\"], [role=\"menuitemcheckbox\"], ",
        "[role=\"option\"], [role=\"tab\"], [role=\"radio\"], [role=\"treeitem\"]"
    )
    .to_string()
}

/// Virtual-mode items default to `[role="option"]` (WAI-ARIA
/// combobox/listbox pattern), still overridable via the
/// `items.<selector>` modifier from RFC-022.
fn parse_items_selector_virtual(modifiers: &[&str]) -> String {
    for (i, m) in modifiers.iter().enumerate() {
        if *m == "items" {
            if let Some(s) = modifiers.get(i + 1) {
                return (*s).to_string();
            }
        }
    }
    "[role=\"option\"]".to_string()
}

/// Public entry point so Pine primitives (PineCombobox) can
/// install virtual-mode behaviour programmatically when the
/// listbox id is only known at component mount time. Passes
/// through orientation/nowrap/items modifiers if provided.
pub fn install_virtual_on(
    host: &Element,
    listbox_id: &str,
    orientation_modifiers: &[String],
    items_selector: Option<&str>,
) {
    let orientation = parse_orientation(orientation_modifiers);
    let wrap = !orientation_modifiers.iter().any(|m| m == "nowrap");
    let items = items_selector
        .map(|s| s.to_string())
        .unwrap_or_else(|| "[role=\"option\"]".to_string());
    install_virtual(host, listbox_id.to_string(), orientation, wrap, items);
}

/// Activedescendant installer — RFC-034.
///
/// Wires arrow-key navigation on `host`, scoped to the listbox
/// with id `listbox_id`. Items inside that listbox receive a
/// `data-highlighted="true"` attribute on the active one; the
/// host element's `aria-activedescendant` tracks the active
/// item's id. DOM focus is never moved.
fn install_virtual(
    host: &Element,
    listbox_id: String,
    orientation: Orientation,
    wrap: bool,
    items_selector: String,
) {
    // Seed: pick the first enabled visible item so a11y trees
    // have an initial activedescendant the moment the host
    // gets focused.
    {
        let items = query_virtual_items(&listbox_id, &items_selector);
        if let Some(first) = items.iter().find(|i| is_virtual_nav_candidate(i)) {
            let id = ensure_id(first, &listbox_id, 0);
            let _ = host.set_attribute("aria-activedescendant", &id);
            set_highlighted(&items, Some(first));
        }
    }

    let host_clone = host.clone();
    let closure: Closure<dyn FnMut(KeyboardEvent)> = Closure::wrap(Box::new({
        move |ev: KeyboardEvent| {
            let action = match classify_key(&ev.key(), orientation) {
                Some(a) => a,
                None => return,
            };
            let items = query_virtual_items(&listbox_id, &items_selector);
            // Enabled AND visible cycle — hidden items (display:none
            // from `pp-show`, `[hidden]`, `aria-hidden="true"`) never
            // receive the virtual highlight so users can't activate
            // an invisible filtered-out match.
            let nav: Vec<Element> = items
                .iter()
                .filter(|i| is_virtual_nav_candidate(i))
                .cloned()
                .collect();
            if nav.is_empty() {
                let _ = host_clone.remove_attribute("aria-activedescendant");
                return;
            }
            let current_id = host_clone.get_attribute("aria-activedescendant");
            let cur_idx = current_id
                .as_ref()
                .and_then(|id| nav.iter().position(|e| e.id() == *id));
            let last = nav.len() - 1;
            let target_idx = match action {
                KeyAction::Next => match cur_idx {
                    Some(i) if i < last => i + 1,
                    Some(_) => {
                        if wrap {
                            0
                        } else {
                            last
                        }
                    }
                    None => 0,
                },
                KeyAction::Prev => match cur_idx {
                    Some(0) => {
                        if wrap {
                            last
                        } else {
                            0
                        }
                    }
                    Some(i) => i - 1,
                    None => last,
                },
                KeyAction::First => 0,
                KeyAction::Last => last,
            };
            ev.prevent_default();
            let target = &nav[target_idx];
            let id = ensure_id(target, &listbox_id, target_idx);
            let _ = host_clone.set_attribute("aria-activedescendant", &id);
            set_highlighted(&items, Some(target));
        }
    })
        as Box<dyn FnMut(KeyboardEvent)>);

    let target: &web_sys::EventTarget = host.as_ref();
    let _ = target.add_event_listener_with_callback("keydown", closure.as_ref().unchecked_ref());
    closure.forget();
    let _ = Reflect::set(host.as_ref(), &STATE_KEY.into(), &JsValue::TRUE);
}

fn query_virtual_items(listbox_id: &str, selector: &str) -> Vec<Element> {
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return Vec::new();
    };
    let Some(listbox) = doc.get_element_by_id(listbox_id) else {
        return Vec::new();
    };
    query_items(&listbox, selector)
}

/// `true` when the item is both enabled AND visible — the
/// candidate pool for virtual-mode navigation.
fn is_virtual_nav_candidate(item: &Element) -> bool {
    !is_item_disabled(item) && is_item_visible(item)
}

fn is_item_visible(item: &Element) -> bool {
    if item.has_attribute("hidden") {
        return false;
    }
    if item.get_attribute("aria-hidden").as_deref() == Some("true") {
        return false;
    }
    // `offsetParent === null` catches `display:none` + display:none
    // ancestors (the common `pp-show` case). A follow-up computed-
    // style check covers fixed-position edge cases.
    if let Ok(html) = item.clone().dyn_into::<HtmlElement>() {
        if html.offset_parent().is_none() {
            if let Some(win) = web_sys::window() {
                if let Ok(Some(style)) = win.get_computed_style(item) {
                    if let Ok(display) = style.get_property_value("display") {
                        if display == "none" {
                            return false;
                        }
                    }
                }
            } else {
                return false;
            }
        }
    }
    true
}

fn ensure_id(el: &Element, listbox_id: &str, index: usize) -> String {
    let existing = el.id();
    if !existing.is_empty() {
        return existing;
    }
    // Keep ids stable per (listbox, index) so repeated queries
    // see the same string.
    let stamp = format!("pine-roving-{listbox_id}-{index}");
    let _ = el.set_attribute("id", &stamp);
    let _ = el.set_attribute("data-pine-roving-id", &stamp);
    stamp
}

fn set_highlighted(items: &[Element], active: Option<&Element>) {
    for item in items {
        if let Some(a) = active {
            if item.is_same_node(Some(a.as_ref())) {
                let _ = item.set_attribute("data-highlighted", "true");
                continue;
            }
        }
        if item.has_attribute("data-highlighted") {
            let _ = item.remove_attribute("data-highlighted");
        }
    }
}

fn query_items(container: &Element, selector: &str) -> Vec<Element> {
    let Ok(list) = container.query_selector_all(selector) else {
        return Vec::new();
    };
    node_list_to_elements(&list)
}

fn node_list_to_elements(list: &NodeList) -> Vec<Element> {
    let len = list.length();
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        if let Some(node) = list.item(i) {
            if let Ok(el) = node.dyn_into::<Element>() {
                out.push(el);
            }
        }
    }
    out
}

fn is_item_disabled(item: &Element) -> bool {
    item.get_attribute("aria-disabled").as_deref() == Some("true") || item.has_attribute("disabled")
}

fn active_element() -> Option<HtmlElement> {
    let window = web_sys::window()?;
    let doc = window.document()?;
    let active = doc.active_element()?;
    active.dyn_into::<HtmlElement>().ok()
}

/// Read the original `pp-roving` attribute value off the element.
/// `DirectiveCall::arg` is consumed once per call; this lets the
/// entry-transfer closure see the listbox id without capturing
/// the full `DirectiveCall`.
fn read_roving_arg(el: &Element) -> Option<String> {
    // The mount strips the pp-roving attribute after dispatch in
    // `release_subtree` — but at install time it's still here.
    // Scan attributes for `pp-roving:<id>`.
    let attrs = el.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        if let Some(rest) = a.name().strip_prefix("pp-roving:") {
            if let Some((id, _mods)) = rest.split_once('.') {
                return Some(id.to_string());
            }
            return Some(rest.to_string());
        }
        // Shorthand form: `@roving:<id>` won't exist because `@` is
        // for event names only; `:roving:<id>` isn't a thing either.
    }
    None
}
