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
use crate::expr::{self, Spanned};
use crate::reactive::effect;
use crate::walker::{self, bind_borrowed_scope_to, track_effect_on};

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
    let template_el: Element = call.el.clone();
    let ast: Spanned<expr::Expr> = match expr::parse(&call.value) {
        Ok(a) => a,
        Err(e) => {
            console::error_1(&JsValue::from_str(&format!(
                "pp-if: {} (at {}..{})",
                e.message, e.span.start, e.span.end
            )));
            return;
        }
    };

    // Resolve the teleport target + enclosing scope once at setup.
    // Both are stable for the lifetime of the template host.
    let teleport_target: Option<Element> = template_el
        .get_attribute("pp-teleport")
        .as_deref()
        .and_then(teleport::resolve_target);
    // Pin the owning scope on every pp-if clone regardless of
    // teleport. Without pinning, an inline clone's enclosing
    // scope is resolved by the walker via DOM ancestry — which
    // skips the `<template>` (the actual scope-carrying element
    // for Pine components whose root directive is pp-if) and
    // lands on whatever component contains the consumer. That
    // breaks `<slot>` materialisation inside the clone (it
    // matches a slot store on the wrong scope) and any
    // directive that reads from the owning scope's proxy.
    let pinned_scope = walker::enclosing_scope(&template_el);

    let current: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(None));

    let effect_id = effect(move || {
        let truthy = expr::evaluate_truthy(&ast, &parent_proxy);

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
                // against the intended proxy. Borrow-mode: removing
                // the clone must not evict the component's scope.
                if let Some((scope_id, proxy)) = pinned_scope.as_ref() {
                    bind_borrowed_scope_to(&clone_root, *scope_id, proxy);
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
                    // Back-link teleport clones to their origin so
                    // consumers (e.g. Pine overlays) can walk from
                    // inside the clone to the host's template slot.
                    if teleport_target.is_some() {
                        let _ = js_sys::Reflect::set(
                            clone_root.as_ref(),
                            &teleport::TELEPORT_ORIGIN_KEY.into(),
                            template_el.as_ref(),
                        );
                        // Record the clone on the template so
                        // release_subtree finds and removes it when
                        // the template's enclosing subtree is torn
                        // down (e.g. a parent pp-if clone containing
                        // this template gets removed from body).
                        teleport::stash(&template_el, &clone_root);
                    }
                    walker::walk(&clone_root);
                    *current.borrow_mut() = Some(clone_root.clone());
                    // Subtree dispatch — Pine compounds (Dialog,
                    // Popover, …) stamp preset attrs on inner custom
                    // children (`<pine-dialog-content>`), not on the
                    // portal clone root, so walking the subtree picks
                    // those up alongside any author-set attrs on the
                    // root itself.
                    transition::enter_subtree(&clone_root, || {});
                }
            }
            (true, Some(clone)) => {
                // Idle / already showing: no-op. Mid-leave: cancel
                // and play enter on the same clone (subtree-wide so
                // all primed leaves get reversed in lock-step).
                if transition::is_subtree_leaving(&clone) {
                    transition::enter_subtree(&clone, || {});
                }
            }
            (false, Some(clone)) => {
                if transition::is_subtree_leaving(&clone) {
                    // Already unmounting; leave fires the removal.
                    return;
                }
                let clone_cap = clone.clone();
                let slot_cap = current.clone();
                let template_cap = template_el.clone();
                let teleported = teleport_target.is_some();
                transition::leave_subtree(&clone, move || {
                    if let Some(parent) = clone_cap.parent_node() {
                        let _ = parent.remove_child(&clone_cap);
                    }
                    // We removed the clone ourselves; drop the
                    // teleport stash so a later release_subtree
                    // doesn't try to remove an already-detached node.
                    if teleported {
                        teleport::clear_stash(&template_cap);
                    }
                    // Explicitly release the subtree we created. The
                    // root MutationObserver may not cover the teleport
                    // target (e.g. we inserted into `<body>` but the
                    // observer is rooted at an embedded host), so
                    // relying on it would orphan nested teleported
                    // clones inside this one. Direct release fires
                    // scope unmount hooks and cascades teleport::release
                    // on every descendant deterministically.
                    walker::release_subtree(clone_cap.as_ref());
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
