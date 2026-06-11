//! Inline `{{expr}}` text interpolation — RFC-025 as revised by RFC-040.
//!
//! The macro parses each direct Text child into static + dynamic
//! segments at compile time (RFC-058 Phase 6.2); the plan applier
//! hands the segment list to [`install_planned_target`]. Each
//! dynamic segment gets its own text node bound by a reactive
//! effect, pinned to the enclosing element so release follows the
//! existing unmount path.
//!
//! Double-brace form means single `{` / `}` in ordinary text is
//! always literal — code samples paste in untouched. Literal
//! `{{` / `}}` in text is rare but supported via backslash escape
//! (`\{{` → `{{`).
//!
//! Interpolation and `pp-text` are intentionally orthogonal:
//! `pp-text` owns the whole element, `{{expr}}` interpolates into
//! surrounding literal text. The macro skips elements that carry
//! `pp-text` so the directive's `textContent` write isn't clobbered
//! by interpolated siblings.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{console, Element, Node, Text};

use crate::expr::{self, Spanned};
use crate::mount::track_effect_on;
use crate::reactive::effect;

enum Segment {
    Static(String),
    Dynamic(String),
}

/// Static-lifetime equivalent of [`Segment`] for compile-time
/// emitted `{{expr}}` interpolation (RFC-058 Phase 6.2).
///
/// The macro parses text-node children at compile time, lifts
/// the resulting segment list into a [`crate::directives::for_plan::StaticInterp`]
/// entry, and the runtime applier hands it to
/// [`install_planned_target`].
#[doc(hidden)]
pub enum PlannedSegment {
    Static(&'static str),
    Dynamic(&'static str),
}

/// Resolve the `text_index`-th text-node child of `parent`
/// (skipping element / comment children). Returns `None` when
/// the index is out of range — DOM edits between macro emission
/// and apply degrade silently rather than panicking.
///
/// Callers that drive multiple [`install_planned_target`] calls
/// against the same parent must resolve every target via this
/// helper **before** any install runs, since each install
/// inserts new text-node siblings and removes the placeholder —
/// reading `text_index` against the live list after a sibling
/// install lands on the wrong node.
#[doc(hidden)]
pub fn resolve_text_target(parent: &Element, text_index: usize) -> Option<Text> {
    let nodes = parent.child_nodes();
    let mut seen: usize = 0;
    for i in 0..nodes.length() {
        let Some(n) = nodes.item(i) else { continue };
        if n.node_type() != Node::TEXT_NODE {
            continue;
        }
        if seen == text_index {
            return n.dyn_into::<Text>().ok();
        }
        seen += 1;
    }
    None
}

/// Install a compile-time emitted segment list against a
/// pre-resolved text node within `parent` (RFC-058 Phase 6.2).
///
/// Use [`resolve_text_target`] to obtain `target` from the
/// macro-emitted `text_index` ahead of time. Resolving and
/// installing in lockstep (one entry at a time) is unsafe when
/// multiple entries share a parent: each install mutates the
/// live text-node list, invalidating later indices.
#[doc(hidden)]
pub fn install_planned_target(
    parent: &Element,
    proxy: &JsValue,
    root: Option<crate::expr::RootAccess>,
    target: &Text,
    segments: &'static [PlannedSegment],
) {
    let runtime_segments: Vec<Segment> = segments
        .iter()
        .map(|s| match s {
            PlannedSegment::Static(t) => Segment::Static((*t).to_string()),
            PlannedSegment::Dynamic(src) => Segment::Dynamic((*src).to_string()),
        })
        .collect();
    install(parent, proxy, root, target, runtime_segments);
}

fn install(
    parent: &Element,
    proxy: &JsValue,
    root: Option<crate::expr::RootAccess>,
    original: &Text,
    segments: Vec<Segment>,
) {
    let parent_node: &Node = parent.as_ref();
    for seg in segments {
        match seg {
            Segment::Static(s) => {
                let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                    return;
                };
                let node = doc.create_text_node(&s);
                let _ = parent_node.insert_before(node.as_ref(), Some(original.as_ref()));
            }
            Segment::Dynamic(src) => {
                let ast: Spanned<expr::Expr> = match expr::parse_cached(&src) {
                    Ok(a) => a,
                    Err(e) => {
                        console::error_1(&JsValue::from_str(&format!(
                            "interpolation `{{{{{src}}}}}`: {} (at {}..{})",
                            e.message, e.span.start, e.span.end
                        )));
                        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                            return;
                        };
                        let fallback = doc.create_text_node(&format!("{{{{{src}}}}}"));
                        let _ =
                            parent_node.insert_before(fallback.as_ref(), Some(original.as_ref()));
                        continue;
                    }
                };
                let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
                    return;
                };
                let node = doc.create_text_node("");
                let _ = parent_node.insert_before(node.as_ref(), Some(original.as_ref()));

                let proxy = proxy.clone();
                let root = root.clone();
                let node_clone = node.clone();
                let id = effect(move || {
                    let v = expr::evaluate_with(&ast, &proxy, root.as_ref());
                    node_clone.set_data(&js_to_string(&v));
                });
                track_effect_on(parent, id);
            }
        }
    }
    // Remove the original un-split text node.
    let _ = parent_node.remove_child(original.as_ref());
}

fn js_to_string(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    v.as_string()
        .or_else(|| v.as_f64().map(crate::text::js_number_string))
        .or_else(|| v.as_bool().map(|b| b.to_string()))
        .unwrap_or_else(|| {
            js_sys::JSON::stringify(v)
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default()
        })
}
