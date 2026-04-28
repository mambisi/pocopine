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
use crate::expr::{self, Spanned};
use crate::mount::{self, bind_borrowed_scope_to, track_effect_on};
use crate::reactive::effect;

/// Compiled-path entry point. Skips the `<template>` cast +
/// expression parse — both already done at compile time by the
/// macro.
///
/// Called by `apply_static_plan` for every `StaticIfPlan` entry
/// the classifier emitted. RFC-058 Phase 4.1a — extracted so the
/// runtime mount dispatch path and the plan applier share one
/// effect-install body instead of growing two parallel
/// implementations of the truthy-toggle / transition / teleport
/// orchestration.
///
/// `body_fn` is the macro-emitted body fragment. With the
/// runtime mount gone (RFC-058 Phase 6.5), every plan-eligible
/// `pp-if` site must carry a fragment; missing fragments
/// surface through the fail-fast plan-failure counter and
/// render an empty subtree.
pub fn install(
    template: HtmlTemplateElement,
    parent_proxy: JsValue,
    ast: Spanned<expr::Expr>,
    body_fn: Option<crate::directives::for_plan::IfBodyFn>,
    teleport_selector: Option<&str>,
) {
    let template_el: Element = template.clone().into();
    let track_anchor = template_el.clone();

    // Resolve the teleport target + enclosing scope once at setup.
    // Both are stable for the lifetime of the template host.
    let teleport_selector = teleport_selector
        .map(str::to_string)
        .or_else(|| template_el.get_attribute("pp-teleport"));
    let teleport_target: Option<Element> = teleport_selector
        .as_deref()
        .and_then(teleport::resolve_target);
    // Pin the owning scope on every pp-if clone regardless of
    // teleport. Without pinning, an inline clone's enclosing
    // scope is resolved by the mount via DOM ancestry — which
    // skips the `<template>` (the actual scope-carrying element
    // for Pine components whose root directive is pp-if) and
    // lands on whatever component contains the consumer. That
    // breaks `<slot>` materialisation inside the clone (it
    // matches a slot store on the wrong scope) and any
    // directive that reads from the owning scope's proxy.
    let pinned_scope = mount::enclosing_scope(&template_el);
    // RFC-027 inject chain — when the controller template is
    // authored in slot content, its enclosing scope is the slot
    // AUTHOR (so directives bind against the right proxy) but
    // descendants' inject parent must chain through the slot
    // OWNER. The slot materialiser stamps `CTX_PARENT_KEY` on
    // the top-level slot child; walk ancestors so a controller
    // wrapped under that top-level child still finds it.
    let inject_parent_id_override = mount::inherited_ctx_parent_of(&template_el);

    let current: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(None));

    let effect_id = effect(move || {
        let truthy = expr::evaluate_truthy(&ast, &parent_proxy);

        let existing = current.borrow().clone();
        match (truthy, existing) {
            (true, None) => {
                // RFC-058 Phase 4.1d body fragment fast-path —
                // when the macro emitted a body fragment, build
                // the clone root via that fragment (which calls
                // `apply_static_plan` against the parent scope
                // to install every directive without going
                // through `mount::walk`). Otherwise fall back
                // to the legacy `clone_template_body` +
                // `mount::walk` path.
                let (clone_root, fragment_built) = match body_fn {
                    Some(f) => {
                        // The fragment installs directives against
                        // the parent scope at construction time, so
                        // we need the pinned scope's id + proxy
                        // available. The fragment is opt-in only
                        // when pinned_scope resolves (otherwise
                        // we have no scope to install against).
                        let Some((scope_id, scope_proxy)) = pinned_scope.as_ref() else {
                            console::error_1(&JsValue::from_str(
                                "pp-if: body fragment requires an enclosing scope",
                            ));
                            return;
                        };
                        let ctx_parent_id = inject_parent_id_override.unwrap_or(*scope_id);
                        match f(*scope_id, scope_proxy, ctx_parent_id) {
                            Some(root) => (root, true),
                            None => {
                                console::error_1(&JsValue::from_str(
                                    "pp-if: body fragment failed to materialise root",
                                ));
                                return;
                            }
                        }
                    }
                    None => match clone_template_body(&template) {
                        Some(root) => (root, false),
                        None => {
                            console::error_1(&JsValue::from_str(
                                "pp-if: <template> body must contain exactly one element",
                            ));
                            return;
                        }
                    },
                };

                // Pin the owning scope onto the clone BEFORE walking
                // so teleported content still resolves directives
                // against the intended proxy. Borrow-mode: removing
                // the clone must not evict the component's scope.
                //
                // For the fragment path the directives already
                // installed against the parent scope, but the
                // `bind_borrowed_scope_to` stamp is still needed
                // so `enclosing_scope` resolves to the parent for
                // any subsequent mount pass (e.g. devtools).
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
                    // Walk the cleaned fragment after insertion so
                    // preserved fallback directives inside nested
                    // custom-component templates still install. The
                    // macro stripped plan-owned attributes, and child
                    // mounts carry `__pp_mounted`, so this does not
                    // duplicate generated installs.
                    if fragment_built {
                        mount::finalize_compiled_subtree(&clone_root);
                    } else {
                        // RFC-058 Phase 6.5 — body must come from
                        // the macro fragment now that the runtime
                        // mount is gone. Surface the
                        // misclassification through the fail-fast
                        // counter so the test harness catches a
                        // plan emitting `pp-if` without a compiled
                        // body.
                        crate::templates_plan::record_plan_failure();
                    }
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
                    mount::release_subtree(clone_cap.as_ref());
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

    track_effect_on(&track_anchor, effect_id);
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
