//! `pp-bind:attr="field"` — bind an HTML attribute (or a child component's
//! prop) to a scope field.
//!
//! Two branches inside the effect body:
//!
//! * **Prop write** — when the target element is a registered component
//!   tag, the value is written to the child's proxy field. That flows
//!   through the child's `set` trap and triggers the child's effects.
//! * **Attribute write** — otherwise, follow upstream Alpine semantics
//!   (`class` / `style` special-cased for string-or-object, everything
//!   else a plain `setAttribute`).
//!
//! Both branches memoise the last-applied value so no-op effect ticks
//! (a watch firing whose *input* changed but whose *bound output*
//! didn't) skip the DOM write. A `setAttribute` is cheap but a
//! `class`/`style` mutation triggers a style recalc; eliding those
//! when nothing changed saves the most expensive browser work.

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::Element;

use crate::mount::track_effect_on;
use crate::reactive::effect;

fn normalize_prop_name(name: &str) -> String {
    name.replace('-', "_")
}

/// Install a `pp-bind:<attr>` effect on `el` that re-evaluates
/// `expr` whenever its reactive dependencies change and applies
/// the result to the target.
///
/// Two branches inside the effect body:
///
/// * **Prop write** — when `el` is a registered child-component
///   tag, the value writes through to the child's proxy. RFC-031
///   guards against parents writing `#[state]` fields.
/// * **Attribute write** — otherwise, follows the upstream
///   Alpine `class` / `style` / plain-attr shape with last-value
///   memoisation.
///
/// Cleanup-safe install entry point.
#[doc(hidden)]
pub fn install_eval(
    el: &Element,
    parent_proxy: &JsValue,
    attr: &str,
    evaluator: Rc<dyn Fn(&JsValue) -> JsValue>,
) {
    let el_owned = el.clone();
    let parent_proxy_owned = parent_proxy.clone();
    let attr_owned = attr.to_string();
    // Capture child-component target info at install time. The
    // scope id is stable for the lifetime of the element; we use
    // it at each effect tick to consult `is_prop` on the child's
    // state so parents can't write through to `#[state]` fields.
    // RFC-096 S1 — only the child's scope ID is captured; the
    // write goes through the scoped writer, so no proxy is ever
    // minted onto the (possibly W3b-elided) child.
    let child_target = crate::mount::child_component_scope_id(el);
    let child_field = normalize_prop_name(attr);

    // Memo of the last value written to this attribute. Serialised
    // to a String so the compare is cheap + monomorphic (class and
    // style have to build a string anyway before `set_attribute`;
    // for plain attrs we serialise to the value we'd write).
    let prev: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    let id = effect(move || {
        let v = evaluator(&parent_proxy_owned);
        match &child_target {
            Some(child_scope_id) => {
                let target_field =
                    crate::model_runtime::resolve_model_key(*child_scope_id, &child_field)
                        .unwrap_or_else(|| child_field.clone());
                // RFC-031 — only `#[prop]` fields are writable
                // from the parent. Silently drop writes to
                // state fields so accidental `<pine-thing
                // loaded="true">` doesn't clobber child state.
                let is_prop = crate::scope::Scope::find(*child_scope_id)
                    .map(|s| s.state.borrow().is_prop(&target_field))
                    .unwrap_or(false);
                if !is_prop {
                    return;
                }
                crate::model_runtime::with_write_origin(
                    crate::model_runtime::WriteOrigin::ParentModelIn,
                    || {
                        // RFC-096 S1 — the scoped writer: same
                        // body the child's set trap delegates to.
                        let _ = crate::scope::write_field(*child_scope_id, &target_field, &v);
                    },
                );
            }
            None => apply_memoised(&el_owned, &attr_owned, &v, &prev),
        }
    });
    track_effect_on(el, id);
}

/// [`apply`] wrapped with a last-value memo. Skips `set_attribute`
/// / `remove_attribute` when the serialised form matches the last
/// write.
fn apply_memoised(el: &Element, attr: &str, v: &JsValue, prev: &Rc<RefCell<Option<String>>>) {
    let dom_attr = dom_attr_name(el, attr);
    let dom_attr = dom_attr.as_ref();
    // Shape handling diverges on whether this is a *state* attribute
    // (`data-*` / `aria-*`) or a classic HTML attribute, but ONLY
    // on the truthy side:
    //
    //   - State attrs render bool `true` as the literal string
    //     `"true"` so CSS value selectors like
    //     `[data-disabled="true"]` and ARIA consumers reading the
    //     value work without an author ternary. `aria-expanded`,
    //     `aria-hidden`, etc. all read by value.
    //   - Classic attrs keep upstream Alpine semantics: bool `true`
    //     renders present-with-empty string (`<input disabled>`).
    //
    // On the falsy side (bool `false`, `null`, `undefined`), both
    // paths REMOVE the attribute — presence-based CSS selectors
    // like `[data-disabled]` or `[hidden]` treat the attribute as a
    // "truthy if present" signal, and rendering `data-disabled="false"`
    // would falsely match them. The demo's Pine stylesheet mixes
    // both idioms (`[data-disabled]` on some primitives,
    // `[data-disabled="true"]` on others), so removal is the only
    // safe falsy shape across both.
    //
    // The removal path is memoed by storing an `Option<String>` where
    // `None` means "attribute is currently absent". A transition into
    // or out of the "absent" state must fire a DOM call exactly once.
    let should_remove = v.is_undefined() || v.is_null() || v == &JsValue::FALSE;
    if should_remove {
        let mut p = prev.borrow_mut();
        if p.is_none() {
            return;
        }
        *p = None;
        let _ = el.remove_attribute(dom_attr);
        if dom_attr != attr {
            let _ = el.remove_attribute(attr);
        }
        return;
    }
    // Compute the string we'd write. For class/style object form,
    // that string IS the joined output; for simple values it's the
    // attribute literal.
    let serialised: String = match attr {
        "class" => match serialise_class(v) {
            Some(s) => s,
            None => return, // shape we don't handle — leave DOM alone
        },
        "style" => match serialise_style(v) {
            Some(s) => s,
            None => return,
        },
        _ => match serialise_plain(attr, v) {
            Some(s) => s,
            None => return,
        },
    };
    {
        let p = prev.borrow();
        if p.as_deref() == Some(serialised.as_str()) {
            return;
        }
    }
    // Write + memo.
    if dom_attr != attr {
        let _ = el.remove_attribute(attr);
    }
    let _ = el.set_attribute(dom_attr, &serialised);
    *prev.borrow_mut() = Some(serialised);
}

const SVG_NS: &str = "http://www.w3.org/2000/svg";

fn dom_attr_name<'a>(el: &Element, attr: &'a str) -> Cow<'a, str> {
    if el.namespace_uri().as_deref() != Some(SVG_NS) {
        return Cow::Borrowed(attr);
    }

    canonical_svg_attr(attr)
        .map(Cow::Borrowed)
        .unwrap_or(Cow::Borrowed(attr))
}

fn canonical_svg_attr(attr: &str) -> Option<&'static str> {
    match attr {
        "attributename" => Some("attributeName"),
        "attributetype" => Some("attributeType"),
        "basefrequency" => Some("baseFrequency"),
        "baseprofile" => Some("baseProfile"),
        "calcmode" => Some("calcMode"),
        "clippathunits" => Some("clipPathUnits"),
        "diffuseconstant" => Some("diffuseConstant"),
        "edgemode" => Some("edgeMode"),
        "filterunits" => Some("filterUnits"),
        "glyphref" => Some("glyphRef"),
        "gradienttransform" => Some("gradientTransform"),
        "gradientunits" => Some("gradientUnits"),
        "kernelmatrix" => Some("kernelMatrix"),
        "kernelunitlength" => Some("kernelUnitLength"),
        "keypoints" => Some("keyPoints"),
        "keysplines" => Some("keySplines"),
        "keytimes" => Some("keyTimes"),
        "lengthadjust" => Some("lengthAdjust"),
        "limitingconeangle" => Some("limitingConeAngle"),
        "markerheight" => Some("markerHeight"),
        "markerunits" => Some("markerUnits"),
        "markerwidth" => Some("markerWidth"),
        "maskcontentunits" => Some("maskContentUnits"),
        "maskunits" => Some("maskUnits"),
        "numoctaves" => Some("numOctaves"),
        "pathlength" => Some("pathLength"),
        "patterncontentunits" => Some("patternContentUnits"),
        "patterntransform" => Some("patternTransform"),
        "patternunits" => Some("patternUnits"),
        "pointsatx" => Some("pointsAtX"),
        "pointsaty" => Some("pointsAtY"),
        "pointsatz" => Some("pointsAtZ"),
        "preservealpha" => Some("preserveAlpha"),
        "preserveaspectratio" => Some("preserveAspectRatio"),
        "primitiveunits" => Some("primitiveUnits"),
        "refx" => Some("refX"),
        "refy" => Some("refY"),
        "repeatcount" => Some("repeatCount"),
        "repeatdur" => Some("repeatDur"),
        "requiredextensions" => Some("requiredExtensions"),
        "requiredfeatures" => Some("requiredFeatures"),
        "specularconstant" => Some("specularConstant"),
        "specularexponent" => Some("specularExponent"),
        "spreadmethod" => Some("spreadMethod"),
        "startoffset" => Some("startOffset"),
        "stddeviation" => Some("stdDeviation"),
        "surfacescale" => Some("surfaceScale"),
        "systemlanguage" => Some("systemLanguage"),
        "tablevalues" => Some("tableValues"),
        "targetx" => Some("targetX"),
        "targety" => Some("targetY"),
        "textlength" => Some("textLength"),
        "viewbox" => Some("viewBox"),
        "viewtarget" => Some("viewTarget"),
        "xchannelselector" => Some("xChannelSelector"),
        "ychannelselector" => Some("yChannelSelector"),
        "zoomandpan" => Some("zoomAndPan"),
        _ => None,
    }
}

/// `data-*` / `aria-*` — attributes read *by value* (CSS selectors,
/// ARIA consumers) rather than by presence. Bool values on these
/// render as the literal strings `"true"` / `"false"` so expressions
/// like `:data-selected="is_selected"` work without an explicit
/// `? 'true' : 'false'` ternary at the call site.
fn is_state_attr(attr: &str) -> bool {
    attr.starts_with("data-") || attr.starts_with("aria-")
}

fn serialise_class(v: &JsValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        return Some(s);
    }
    if v.is_object() {
        let obj: Object = v.clone().unchecked_into();
        let keys = Object::keys(&obj);
        let mut out: Vec<String> = Vec::new();
        for i in 0..keys.length() {
            let k = keys.get(i);
            let truthy = Reflect::get(&obj, &k)
                .map(|val| val.as_bool().unwrap_or(!val.is_falsy()))
                .unwrap_or(false);
            if truthy
                && let Some(s) = k.as_string() {
                    out.push(s);
                }
        }
        return Some(out.join(" "));
    }
    None
}

fn serialise_style(v: &JsValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        return Some(s);
    }
    if v.is_object() {
        let obj: Object = v.clone().unchecked_into();
        let keys = Object::keys(&obj);
        let mut out = String::new();
        for i in 0..keys.length() {
            let k = keys.get(i);
            if let (Some(name), Ok(val)) = (k.as_string(), Reflect::get(&obj, &k)) {
                let val_s = val.as_string().unwrap_or_default();
                out.push_str(&format!("{name}:{val_s};"));
            }
        }
        return Some(out);
    }
    None
}

fn serialise_plain(attr: &str, v: &JsValue) -> Option<String> {
    if let Some(s) = v.as_string() {
        return Some(s);
    }
    if let Some(n) = v.as_f64() {
        return Some(n.to_string());
    }
    if let Some(b) = v.as_bool() {
        // `false` is routed through `remove_attribute` before this
        // function runs, on both state-attr and classic-attr paths.
        // So the only bool that reaches us is `true`: state attrs
        // render it as the literal `"true"` (CSS value selectors +
        // ARIA consumers read by value); classic attrs keep the
        // upstream `true → present-with-empty-string` shape.
        if is_state_attr(attr) {
            return Some(if b { "true".into() } else { "false".into() });
        }
        return Some(String::new());
    }
    // Fallback: JSON-stringify objects/arrays (matches the old
    // behaviour). Silently dropping non-serialisable values would
    // regress — return an empty string on serialisation failure,
    // same as the pre-memo path did.
    Some(
        js_sys::JSON::stringify(v)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_default(),
    )
}
