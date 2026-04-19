//! `pp-teleport="<selector>"` — move a `<template>`'s body to a
//! target location in the DOM. Per RFC-006.
//!
//! Designed for modals, dialogs, popovers, and tooltips: content that
//! needs to escape `overflow: hidden` clipping, `z-index` stacking,
//! and `transform`-induced containing blocks. The teleported clone
//! still binds against the owning component's scope — we pin the
//! scope onto the clone root so `walker::enclosing_scope` returns the
//! intended proxy even after the move.
//!
//! Composes with `pp-if`: if the template has both attributes, `pp-if`
//! owns the mount/unmount cycle and calls back into
//! [`resolve_target`] to pick the insert location. Standalone (no
//! `pp-if`) means "always mount here."

use js_sys::Reflect;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, Element, HtmlTemplateElement, Node};

use super::DirectiveCall;
use crate::walker::{self, bind_borrowed_scope_to};

const TELEPORTED_KEY: &str = "__pp_teleported";

pub fn run(call: &DirectiveCall) {
    // When `pp-if` is also present, that directive owns the mount
    // cycle and consults [`resolve_target`] directly. Standalone
    // teleport always mounts.
    if call.el.has_attribute("pp-if") {
        return;
    }

    let template: HtmlTemplateElement = match call.el.clone().dyn_into() {
        Ok(t) => t,
        Err(_) => {
            console::error_1(&JsValue::from_str(
                "pp-teleport: must be on a <template> element",
            ));
            return;
        }
    };

    let Some(target) = resolve_target(&call.value) else {
        console::error_1(&JsValue::from_str(&format!(
            "pp-teleport: target selector {:?} did not match any element",
            call.value
        )));
        return;
    };

    let Some(clone_root) = clone_template_body(&template) else {
        console::error_1(&JsValue::from_str(
            "pp-teleport: <template> body must contain exactly one element",
        ));
        return;
    };

    // Pin the owning scope onto the clone so directives inside still
    // resolve the intended proxy after the DOM move. The scope is
    // borrowed — removing the clone must not evict the owning
    // component's scope from the registry.
    if let Some((scope_id, proxy)) = walker::enclosing_scope(call.el) {
        bind_borrowed_scope_to(&clone_root, scope_id, &proxy);
    }

    if target.append_child(clone_root.as_ref()).is_ok() {
        walker::walk(&clone_root);
        stash_teleported(call.el, &clone_root);
    }
}

/// Resolve a teleport target selector to a DOM element. `"body"` is
/// a convenience alias for `document.body` since it's the canonical
/// target for dialogs.
pub fn resolve_target(selector: &str) -> Option<Element> {
    let sel = selector.trim();
    let doc = web_sys::window()?.document()?;
    if sel == "body" {
        return doc.body().map(Element::from);
    }
    doc.query_selector(sel).ok().flatten()
}

/// Called by `walker::release_subtree` on every released element. If
/// this element is a template host with a teleported clone, remove
/// the clone so the MutationObserver picks up its subtree cleanup.
pub fn release(el: &Element) {
    let Some(clone) = take_teleported(el) else { return };
    if let Some(parent) = clone.parent_node() {
        let _ = parent.remove_child(&clone);
    }
}

fn stash_teleported(template: &Element, clone: &Element) {
    let _ = Reflect::set(template.as_ref(), &TELEPORTED_KEY.into(), clone.as_ref());
}

fn take_teleported(template: &Element) -> Option<Element> {
    let v = Reflect::get(template.as_ref(), &TELEPORTED_KEY.into()).ok()?;
    if v.is_undefined() || v.is_null() {
        return None;
    }
    let _ = Reflect::set(
        template.as_ref(),
        &TELEPORTED_KEY.into(),
        &JsValue::UNDEFINED,
    );
    v.dyn_into::<Element>().ok()
}

fn clone_template_body(template: &HtmlTemplateElement) -> Option<Element> {
    let fragment: Node = template.content().clone_node_with_deep(true).ok()?;
    let children = fragment.child_nodes();
    for i in 0..children.length() {
        if let Some(n) = children.item(i) {
            if let Ok(el) = n.dyn_into::<Element>() {
                return Some(el);
            }
        }
    }
    None
}
