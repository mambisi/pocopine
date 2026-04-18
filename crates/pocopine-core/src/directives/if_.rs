//! `pp-if="expression"` — conditional render. Lives on a `<template>`
//! host just like `pp-for`.
//!
//! When the expression is truthy and there's no current clone, clone
//! the template body and insert it before the template. When it flips
//! to falsy, remove the clone (which cleans up effects + scopes via
//! the `MutationObserver` path). Unlike `pp-show`, `pp-if` actually
//! unmounts — effects stop running, scopes are released.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, Element, HtmlTemplateElement, Node};

use super::DirectiveCall;
use crate::path::resolve_truthy;
use crate::reactive::effect;
use crate::walker::{self, track_effect_on};

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

    let current: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(None));

    let effect_id = effect(move || {
        let truthy = resolve_truthy(&parent_proxy, &expr);

        let mut slot = current.borrow_mut();
        match (truthy, slot.as_ref()) {
            (true, None) => {
                let Some(clone_root) = clone_template_body(&template) else {
                    console::error_1(&JsValue::from_str(
                        "pp-if: <template> body must contain exactly one element",
                    ));
                    return;
                };
                if let Some(parent_node) = template_el.parent_node() {
                    if parent_node
                        .insert_before(clone_root.as_ref(), Some(template_el.as_ref()))
                        .is_ok()
                    {
                        walker::walk(&clone_root);
                        *slot = Some(clone_root);
                    }
                }
            }
            (false, Some(_)) => {
                if let Some(clone) = slot.take() {
                    if let Some(parent) = clone.parent_node() {
                        let _ = parent.remove_child(&clone);
                    }
                }
            }
            _ => {} // already in the desired state
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
