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

use crate::walker::{self, bind_borrowed_scope_to};

const TELEPORTED_KEY: &str = "__pp_teleported";
/// Backpointer stored on the teleported clone → the original
/// `<template pp-teleport>` element. Lets consumers of a teleported
/// subtree (e.g. Pine overlays that need to dispatch events up
/// through the parent scope) walk back to the host-scope DOM.
pub const TELEPORT_ORIGIN_KEY: &str = "__pp_teleport_origin";

/// Walk from `el` up through ancestor `__pp_teleport_origin`
/// pointers to find the original host element — the parent of
/// the `<template pp-teleport>` whose body was cloned. Returns
/// `None` when `el` is not inside a teleported subtree or the
/// chain can't be resolved.
///
/// Used by overlay components (Dialog, Popover, DropdownMenu)
/// that dispatch `pp:update:model` from the host tag rather than
/// the teleported content, since bubbling from `<body>` wouldn't
/// reach a listener on the original host.
pub fn host_of(el: &Element) -> Option<Element> {
    let origin_key = JsValue::from_str(TELEPORT_ORIGIN_KEY);
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(node) = cur {
        if let Ok(v) = Reflect::get(node.as_ref(), &origin_key) {
            if !v.is_undefined() && !v.is_null() {
                if let Ok(template) = v.dyn_into::<Element>() {
                    return template.parent_element();
                }
            }
        }
        cur = node.parent_element();
    }
    None
}

/// Compiled-path entry point. Skips the `<template>` cast +
/// the `pp-if` co-occurrence check (the macro only graduates
/// pp-teleport sites that pass that check at compile time).
///
/// Called by `apply_static_plan` for every `StaticTeleportPlan`
/// entry the classifier emitted. RFC-058 Phase 4.3 — extracted
/// so the runtime walker dispatch path and the plan applier
/// share one mount body. The owning scope still pins onto the
/// clone via `walker::enclosing_scope` so directives inside the
/// teleported subtree resolve the intended proxy after the DOM
/// move.
///
/// `body_fn` is the optional macro-emitted body fragment from
/// RFC-058 Phase 4.3c. When `Some`, the clone root is built via
/// the fragment (which stamps cleaned HTML + installs every
/// directive against the enclosing scope via the Phase 1
/// helpers — no `walker::walk` involvement). When `None`, the
/// legacy `clone_template_body` + `walker::walk` path runs.
pub fn install(
    template: HtmlTemplateElement,
    selector: &str,
    body_fn: Option<crate::directives::for_plan::TeleportBodyFn>,
) {
    let template_el: Element = template.clone().into();

    let Some(target) = resolve_target(selector) else {
        console::error_1(&JsValue::from_str(&format!(
            "pp-teleport: target selector {selector:?} did not match any element"
        )));
        return;
    };

    let pinned_scope = walker::enclosing_scope(&template_el);
    // See `if_::install` — slot-content controllers must thread
    // the slot owner's `CTX_PARENT_KEY` through to the body
    // fragment's root for nested inject chain resolution.
    let inject_parent_id_override = walker::inherited_ctx_parent_of(&template_el);

    let (clone_root, fragment_built) = match body_fn {
        Some(f) => {
            let Some((scope_id, scope_proxy)) = pinned_scope.as_ref() else {
                console::error_1(&JsValue::from_str(
                    "pp-teleport: body fragment requires an enclosing scope",
                ));
                return;
            };
            let ctx_parent_id = inject_parent_id_override.unwrap_or(*scope_id);
            match f(*scope_id, scope_proxy, ctx_parent_id) {
                Some(root) => (root, true),
                None => {
                    console::error_1(&JsValue::from_str(
                        "pp-teleport: body fragment failed to materialise root",
                    ));
                    return;
                }
            }
        }
        None => match clone_template_body(&template) {
            Some(root) => (root, false),
            None => {
                console::error_1(&JsValue::from_str(
                    "pp-teleport: <template> body must contain exactly one element",
                ));
                return;
            }
        },
    };

    // Pin the owning scope onto the clone so directives inside still
    // resolve the intended proxy after the DOM move. The scope is
    // borrowed — removing the clone must not evict the owning
    // component's scope from the registry.
    if let Some((scope_id, proxy)) = pinned_scope.as_ref() {
        bind_borrowed_scope_to(&clone_root, *scope_id, proxy);
    }

    if target.append_child(clone_root.as_ref()).is_ok() {
        // Back-link the clone to its origin template so consumers
        // can walk from inside the teleport back to the host scope.
        let _ = Reflect::set(
            clone_root.as_ref(),
            &TELEPORT_ORIGIN_KEY.into(),
            template_el.as_ref(),
        );
        // Walk the cleaned fragment after insertion so preserved
        // fallback directives inside nested custom-component
        // templates still install. Plan-owned attributes were
        // stripped by the macro, and generated child mounts are
        // guarded by `__pp_mounted`.
        if fragment_built {
            walker::finalize_compiled_subtree(&clone_root);
        } else {
            // RFC-058 Phase 6.5 — body must come from the macro
            // fragment now that the runtime walker is gone.
            crate::templates_plan::record_plan_failure();
        }
        stash_teleported(&template_el, &clone_root);
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
/// the clone and release its subtree deterministically — a root-
/// MutationObserver may not cover the teleport target (e.g. tests
/// observe a mount host inside `<body>`, but the clone lives on
/// `<body>` itself), so relying on the observer would leak the
/// clone's scopes + effects.
pub fn release(el: &Element) {
    let Some(clone) = take_teleported(el) else {
        return;
    };
    if let Some(parent) = clone.parent_node() {
        let _ = parent.remove_child(&clone);
    }
    walker::release_subtree(clone.as_ref());
}

fn stash_teleported(template: &Element, clone: &Element) {
    let _ = Reflect::set(template.as_ref(), &TELEPORTED_KEY.into(), clone.as_ref());
}

/// Public stash hook for `pp-if` templates that own their own mount
/// cycle but still teleport the clone. Recording the clone here lets
/// [`release`] remove it when the template's enclosing subtree is
/// torn down — without this, a nested `pp-if+pp-teleport` inside a
/// parent `pp-if` clone gets orphaned in the teleport target when
/// the parent clone is removed (the parent's body-removal triggers
/// scope/effect release on the nested template, but the nested
/// clone itself, living separately in the teleport target, has no
/// other hook to trip its removal).
pub(crate) fn stash(template: &Element, clone: &Element) {
    stash_teleported(template, clone);
}

/// Clear a previously stashed clone. Called by `pp-if` when its own
/// leave callback successfully removes the clone, so a subsequent
/// [`release`] doesn't try to remove an already-detached element.
pub(crate) fn clear_stash(template: &Element) {
    let _ = Reflect::set(
        template.as_ref(),
        &TELEPORTED_KEY.into(),
        &JsValue::UNDEFINED,
    );
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
