//! `pp-if="expression"` — conditional render. Lives on a `<template>`
//! host just like `pp-for`.
//!
//! When the expression is truthy and there's no current clone, clone
//! the template body and insert it before the template. When it flips
//! to falsy, remove the clone (which cleans up effects + scopes via
//! the `MutationObserver` path). Unlike `pp-show`, `pp-if` actually
//! unmounts — effects stop running, scopes are released.
//!
//! If the template body has any `pp-transition:*` attributes, mounts
//! and unmounts go through [`crate::directives::transition`] so the
//! enter sequence plays after insert and the remove is deferred until
//! the leave sequence finishes. A re-flip to truthy mid-leave cancels
//! the pending unmount and resumes enter on the same clone.
//!
//! If the template has `pp-teleport`, the insert location is the
//! teleport target instead of "before the template." The clone still
//! binds against the template's enclosing scope (pinned on the clone
//! root) so its `pp-*` directives resolve the intended proxy even
//! after the DOM move.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, Element, HtmlTemplateElement, Node};

use super::teleport;
use super::transition;
use super::DirectiveCall;
use crate::path::resolve_truthy;
use crate::reactive::effect;
use crate::walker::{self, bind_scope_to, track_effect_on};

pub fn run(call: &DirectiveCall) {
    let template: HtmlTemplateElement = match call.el.clone().dyn_into() {
        Ok(t) => t,
        Err(_) => {
            console::error_1(&JsValue::from_str(
                "pp-if: must be on a <template> element",
            ));
            return;
        }
    };

    let parent_proxy = call.proxy.clone();
    let expr = call.value.clone();
    let template_el: Element = call.el.clone();

    // Resolve the teleport target + enclosing scope once at setup.
    // Both are stable for the lifetime of the template host.
    let teleport_target: Option<Element> = template_el
        .get_attribute("pp-teleport")
        .as_deref()
        .and_then(teleport::resolve_target);
    let pinned_scope = if teleport_target.is_some() {
        walker::enclosing_scope(&template_el)
    } else {
        None
    };

    let current: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(None));

    let effect_id = effect(move || {
        let truthy = resolve_truthy(&parent_proxy, &expr);

        let existing = current.borrow().clone();
        match (truthy, existing) {
            (true, None) => {
                let Some(clone_root) = clone_template_body(&template) else {
                    console::error_1(&JsValue::from_str(
                        "pp-if: <template> body must contain exactly one element",
                    ));
                    return;
                };

                // Pin the owning scope onto the clone BEFORE walking
                // so teleported content still resolves directives
                // against the intended proxy.
                if let Some((scope_id, proxy)) = pinned_scope.as_ref() {
                    bind_scope_to(&clone_root, *scope_id, proxy);
                }

                let inserted = match teleport_target.as_ref() {
                    Some(target) => target.append_child(clone_root.as_ref()).is_ok(),
                    None => template_el
                        .parent_node()
                        .map(|p| {
                            p.insert_before(clone_root.as_ref(), Some(template_el.as_ref()))
                                .is_ok()
                        })
                        .unwrap_or(false),
                };

                if inserted {
                    walker::walk(&clone_root);
                    *current.borrow_mut() = Some(clone_root.clone());
                    transition::enter(&clone_root, || {});
                }
            }
            (true, Some(clone)) => {
                // Idle / already showing: no-op. Mid-leave: cancel
                // and play enter on the same clone.
                if transition::is_leaving(&clone) {
                    transition::enter(&clone, || {});
                }
            }
            (false, Some(clone)) => {
                if transition::is_leaving(&clone) {
                    // Already unmounting; leave fires the removal.
                    return;
                }
                let clone_cap = clone.clone();
                let slot_cap = current.clone();
                transition::leave(&clone, move || {
                    if let Some(parent) = clone_cap.parent_node() {
                        let _ = parent.remove_child(&clone_cap);
                    }
                    // Only clear the slot if it still points to this
                    // clone — a rapid re-mount could have replaced it.
                    let mut slot = slot_cap.borrow_mut();
                    if let Some(cur) = slot.as_ref() {
                        if cur.is_same_node(Some(clone_cap.as_ref())) {
                            *slot = None;
                        }
                    }
                });
            }
            (false, None) => {}
        }
    });

    track_effect_on(call.el, effect_id);
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
