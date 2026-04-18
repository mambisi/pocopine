//! `pp-for="item in items"` — iterate an array, clone the host
//! `<template>`'s body once per item, bind each clone against a
//! [`crate::loop_scope::LoopScope`].
//!
//! Requires the host to be a `<template>` element. The content of
//! that template is cloned per iteration; the original template stays
//! in the DOM as a mount anchor. Clones are inserted as siblings
//! before the template. See `rfcs/rfc-004-pp-for.md` for the full
//! spec.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, Element, HtmlTemplateElement, Node};

use super::DirectiveCall;
use crate::loop_scope::LoopScope;
use crate::path::resolve_path;
use crate::reactive::effect;
use crate::scope::Scope;
use crate::walker::{self, bind_scope_to, track_effect_on};

pub fn run(call: &DirectiveCall) {
    // Parse "item in items"
    let Some((item_name, items_expr)) = parse_expr(&call.value) else {
        console::error_1(&JsValue::from_str(&format!(
            "pp-for: expected `<ident> in <path>`, got {:?}",
            call.value
        )));
        return;
    };

    // Host must be <template>.
    let template: HtmlTemplateElement = match call.el.clone().dyn_into() {
        Ok(t) => t,
        Err(_) => {
            console::error_1(&JsValue::from_str(
                "pp-for: must be on a <template> element (see rfc-004)",
            ));
            return;
        }
    };

    let parent_proxy = call.proxy.clone();
    let template_el: Element = call.el.clone();

    // Track the elements we've inserted so we can remove them on
    // re-run. Scope cleanup happens automatically via MutationObserver
    // + `release_subtree` when the DOM nodes are removed.
    let prior: Rc<RefCell<Vec<Element>>> = Rc::new(RefCell::new(Vec::new()));

    let effect_id = effect(move || {
        // Read items. If it isn't an array, render nothing.
        let items_js = resolve_path(&parent_proxy, &items_expr);
        let arr: Array = match items_js.dyn_into::<Array>() {
            Ok(a) => a,
            Err(_) => Array::new(),
        };
        let total = arr.length() as usize;

        // Tear down prior clones.
        {
            let mut prior = prior.borrow_mut();
            for el in prior.drain(..) {
                if let Some(parent) = el.parent_node() {
                    let _ = parent.remove_child(&el);
                }
            }
        }

        // Nothing to do for an empty array.
        if total == 0 {
            return;
        }

        // Anchor node: the template. New clones go *before* it.
        let Some(parent_node) = template_el.parent_node() else {
            return;
        };

        let mut fresh: Vec<Element> = Vec::with_capacity(total);
        for i in 0..total {
            let item = arr.get(i as u32);
            let loop_state = LoopScope {
                item_name: item_name.clone(),
                item,
                index: i,
                total,
                parent: parent_proxy.clone(),
            };
            let scope = Scope::new(Rc::new(RefCell::new(loop_state)));
            let proxy = scope.into_proxy();

            // Clone the <template>.content and pull out its first
            // element child. v0 requires exactly one element in the
            // template body (rfc-004 §5.2).
            let Some(clone_root) = clone_template_body(&template) else {
                console::error_1(&JsValue::from_str(
                    "pp-for: <template> body must contain exactly one element",
                ));
                break;
            };

            // Pin the loop scope onto the clone so its pp-* directives
            // resolve through `LoopScope` (which falls through to the
            // parent for non-loop keys).
            bind_scope_to(&clone_root, scope.id, &proxy);

            // Insert before the template element, in source order.
            if parent_node
                .insert_before(clone_root.as_ref(), Some(template_el.as_ref()))
                .is_ok()
            {
                // Walk the clone so directives bind. The walker picks
                // up the scope we already attached instead of trying
                // to mount a component.
                walker::walk(&clone_root);
                fresh.push(clone_root);
            }
        }

        *prior.borrow_mut() = fresh;
    });

    track_effect_on(call.el, effect_id);
}

/// Clone `<template>.content` deeply and return the first element
/// child of the resulting fragment. Returns `None` when the body is
/// empty or has only non-element nodes.
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

/// Parse `"item in items"`. Returns `None` on anything we don't want
/// to accept in v0 (destructuring, `(i, x) in ...`, empty halves).
fn parse_expr(s: &str) -> Option<(String, String)> {
    let s = s.trim();
    let (lhs, rhs) = s.split_once(" in ")?;
    let ident = lhs.trim();
    let items = rhs.trim();
    if ident.is_empty() || items.is_empty() {
        return None;
    }
    if !ident.chars().all(|c| c.is_alphanumeric() || c == '_')
        || ident.chars().next().is_some_and(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((ident.to_string(), items.to_string()))
}

#[cfg(test)]
mod tests {
    use super::parse_expr;

    #[test]
    fn basic() {
        assert_eq!(
            parse_expr("story in stories"),
            Some(("story".into(), "stories".into()))
        );
    }

    #[test]
    fn dotted_path_on_rhs() {
        assert_eq!(
            parse_expr("child in node.children"),
            Some(("child".into(), "node.children".into()))
        );
    }

    #[test]
    fn strip_whitespace() {
        assert_eq!(
            parse_expr("  foo  in  bar  "),
            Some(("foo".into(), "bar".into()))
        );
    }

    #[test]
    fn rejects_destructuring() {
        assert_eq!(parse_expr("(item, i) in items"), None);
    }

    #[test]
    fn rejects_leading_digit() {
        assert_eq!(parse_expr("1x in items"), None);
    }

    #[test]
    fn rejects_missing_in() {
        assert_eq!(parse_expr("story"), None);
    }
}
