//! RFC-094 — the conditional-chain controller behind
//! `pp-if [pp-else-if…] [pp-else]`.
//!
//! One chain = one `StaticCondPlan` = one effect computing the
//! first-truthy branch index. At most one branch's clone exists
//! at a time, inserted before a single **comment anchor**
//! (`<!--pp:cond-->`) that replaces the authored `<template>` at
//! install — so the live DOM carries no phantom element siblings
//! (Stylekit's sibling selectors and `:nth-child` see only real
//! content).
//!
//! RFC-096 alignment: branch evaluators arrive pre-built on the
//! scoped access (no proxy reads), body fragments install via
//! scope ids, and the controller never mints or dereferences a
//! proxy — chain-only plans are proxy-elision eligible.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{console, Element, HtmlTemplateElement};

use super::{teleport, transition};
use crate::directives::for_plan::IfBodyFn;
use crate::mount::{self, bind_borrowed_scope_to, track_effect_on};
use crate::reactive::{effect, ScopeId};

/// A branch's pre-built evaluator (scoped access inside).
pub type BranchEval = Rc<dyn Fn(&JsValue) -> JsValue>;

/// Install the chain controller. `branches` carries the head
/// (pp-if) followed by every pp-else-if, in authored order;
/// `else_body` is the pp-else, if any. `consumed_count` is how
/// many chain-member `<template>`s follow the head as element
/// siblings — they are detached here (kept alive for the legacy
/// clone fallback of unlifted bodies) before the head is swapped
/// for the comment anchor.
///
/// `scope_id`/`proxy` are the installing scope (the component
/// for template plans, the author scope for slot fragments) —
/// the pin every clone is bound to. The RFC-027 inject-chain
/// override is still resolved from the template's ancestry,
/// before the swap.
#[allow(clippy::too_many_arguments)]
pub fn install_cond(
    template: HtmlTemplateElement,
    scope_id: ScopeId,
    proxy: JsValue,
    branches: Vec<(BranchEval, Option<IfBodyFn>)>,
    has_else: bool,
    else_body: Option<IfBodyFn>,
    consumed_count: u16,
    teleport_selector: Option<&str>,
) {
    let template_el: Element = template.clone().into();

    // Resolve everything that needs the template's DOM position
    // BEFORE the anchor swap.
    let teleport_selector = teleport_selector
        .map(str::to_string)
        .or_else(|| template_el.get_attribute("pp-teleport"));
    let teleport_target: Option<Element> = teleport_selector
        .as_deref()
        .and_then(teleport::resolve_target);
    let inject_parent_id_override = mount::inherited_ctx_parent_of(&template_el);
    let ctx_parent_id = inject_parent_id_override.unwrap_or(scope_id);

    let Some(parent_el) = template_el.parent_element() else {
        return;
    };

    // Detach the consumed chain members (pp-else-if / pp-else
    // templates). Kept alive in `member_templates` so a branch
    // whose body fell outside the lifting envelope can still
    // clone its content (and surface `record_plan_failure`,
    // matching the single-branch pp-if contract).
    let mut member_templates: Vec<Option<HtmlTemplateElement>> = Vec::new();
    let mut sib = template_el.next_element_sibling();
    for _ in 0..consumed_count {
        let Some(s) = sib else { break };
        sib = s.next_element_sibling();
        s.remove();
        member_templates.push(s.dyn_into::<HtmlTemplateElement>().ok());
    }

    // RFC-094 — the comment anchor. After this line the authored
    // `<template>` is out of the live DOM; the controller owns
    // the Comment by reference and never resolves a path again.
    //
    // EXCEPTION: when the template is itself the component's
    // scope-carrying ROOT (the established Pine pattern —
    // `<template pp-if>` as the template root), it must stay:
    // the mount stamped `SCOPE_ID_KEY` on it, and scope
    // resolution / parent prop writes / teardown all reach the
    // component through `first_element_child`. There are no
    // element siblings at that position (it is the host's only
    // child), so the phantom-sibling motivation doesn't apply.
    let is_scope_root = mount::scope_id_of_element(&template_el).is_some();
    let (anchor_node, stash_el): (web_sys::Node, Element) = if is_scope_root {
        (template_el.clone().into(), template_el.clone())
    } else {
        let Some(doc) = template_el.owner_document() else {
            return;
        };
        let anchor = doc.create_comment("pp:cond");
        let anchor_node: web_sys::Node = anchor.into();
        if parent_el
            .replace_child(&anchor_node, template_el.as_ref())
            .is_err()
        {
            return;
        }
        (anchor_node, parent_el.clone())
    };

    drive_cond(
        anchor_node,
        stash_el,
        Some(template),
        member_templates,
        scope_id,
        proxy,
        ctx_parent_id,
        branches,
        has_else,
        else_body,
        teleport_target,
        None, // mount: no seed — the first effect run mounts the branch
    );
}

/// RFC-099 Phase 3 — the hydrate (claim) entry. The server already
/// rendered the active branch's clone followed by the
/// `<!--pp:cond:idx-->` comment anchor (member `<template>`s
/// dropped). Reuse the SAME effect, pre-seeded with the adopted
/// clone + active index so the first run short-circuits (no DOM
/// write); later condition changes drive normal swaps. Future flips
/// always have a macro body fn because the SSR stamper only expands
/// a chain when every branch is liftable.
#[allow(clippy::too_many_arguments)]
pub fn hydrate_cond(
    anchor_node: web_sys::Node,
    active: Option<usize>,
    adopted: Option<Element>,
    scope_id: ScopeId,
    proxy: JsValue,
    branches: Vec<(BranchEval, Option<IfBodyFn>)>,
    has_else: bool,
    else_body: Option<IfBodyFn>,
    teleport_selector: Option<&str>,
) {
    // The effect rides the anchor's parent element (a comment can't
    // carry the effects table).
    let Some(parent_el) = anchor_node
        .parent_node()
        .and_then(|n| n.dyn_into::<Element>().ok())
    else {
        return;
    };
    let teleport_target: Option<Element> = teleport_selector.and_then(teleport::resolve_target);
    let ctx_parent_id = mount::inherited_ctx_parent_of(&parent_el).unwrap_or(scope_id);
    if let Some(el) = adopted.as_ref() {
        bind_borrowed_scope_to(el, scope_id, &proxy);
    }
    drive_cond(
        anchor_node,
        parent_el,
        None, // server dropped the member <template>s
        Vec::new(),
        scope_id,
        proxy,
        ctx_parent_id,
        branches,
        has_else,
        else_body,
        teleport_target,
        Some((active, adopted)),
    );
}

/// Shared effect driver for [`install_cond`] (mount) and
/// [`hydrate_cond`] (claim). `seed` pre-seeds `(prev_active,
/// current)`; `None` is the mount path (first run mounts the active
/// branch), `Some` is the hydrate path (first run short-circuits
/// when the server branch matches the client evaluation).
#[allow(clippy::too_many_arguments)]
fn drive_cond(
    anchor_node: web_sys::Node,
    stash_el: Element,
    head_template: Option<HtmlTemplateElement>,
    member_templates: Vec<Option<HtmlTemplateElement>>,
    scope_id: ScopeId,
    proxy: JsValue,
    ctx_parent_id: ScopeId,
    branches: Vec<(BranchEval, Option<IfBodyFn>)>,
    has_else: bool,
    else_body: Option<IfBodyFn>,
    teleport_target: Option<Element>,
    seed: Option<(Option<usize>, Option<Element>)>,
) {
    let (seed_active, seed_clone) = seed.unwrap_or((None, None));
    let current: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(seed_clone));
    let prev_active: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(seed_active));
    // (branch index, clone) of an outgoing clone whose leave
    // transition is still playing — a re-flip back to that branch
    // cancels the leave and resumes the SAME clone in place (the
    // pre-RFC-094 pp-if contract: no flicker, state preserved).
    #[allow(clippy::type_complexity)]
    let leaving: Rc<RefCell<Option<(Option<usize>, Element)>>> = Rc::new(RefCell::new(None));
    // Effect lifetime: on the kept root template when the
    // template IS the scope root, else on the anchor's parent
    // (a comment can't carry the effects table).
    let track_el = stash_el.clone();

    let effect_id = effect(move || {
        let active = branches
            .iter()
            .position(|(eval, _)| !eval(&proxy).is_falsy())
            .or_else(|| has_else.then_some(branches.len()));

        if active == prev_active.get() && (active.is_none() || current.borrow().is_some()) {
            // Same branch still mounted (or still nothing to
            // mount). Mid-leave on the same branch: cancel and
            // re-enter in lock-step.
            if let Some(clone) = current.borrow().as_ref() {
                if transition::is_subtree_leaving(clone) {
                    transition::enter_subtree(clone, || {});
                }
            }
            return;
        }

        // Mid-leave cancel: flipping BACK to the branch whose
        // clone is still leave-animating resumes that clone
        // instead of mounting a duplicate beside it.
        let resumable = {
            let mut l = leaving.borrow_mut();
            match l.take() {
                Some((idx, el)) if idx == active && el.is_connected() => Some(el),
                other => {
                    *l = other;
                    None
                }
            }
        };
        if let Some(el) = resumable {
            transition::enter_subtree(&el, || {});
            *current.borrow_mut() = Some(el);
            prev_active.set(active);
            return;
        }

        // ── teardown the outgoing clone ──
        let existing = current.borrow().clone();
        if let Some(clone) = existing {
            if !transition::is_subtree_leaving(&clone) {
                let clone_cap = clone.clone();
                let slot_cap = current.clone();
                let stash_cap = stash_el.clone();
                let leaving_cap = leaving.clone();
                let teleported = teleport_target.is_some();
                *leaving.borrow_mut() = Some((prev_active.get(), clone.clone()));
                transition::leave_subtree(&clone, move || {
                    if let Some(parent) = clone_cap.parent_node() {
                        let _ = parent.remove_child(&clone_cap);
                    }
                    if teleported {
                        teleport::clear_stash(&stash_cap, &clone_cap);
                    }
                    mount::release_subtree(clone_cap.as_ref());
                    {
                        let mut l = leaving_cap.borrow_mut();
                        if l.as_ref()
                            .map(|(_, el)| el.is_same_node(Some(clone_cap.as_ref())))
                            .unwrap_or(false)
                        {
                            *l = None;
                        }
                    }
                    // Only clear the slot if it still points at
                    // this clone — a rapid branch switch may have
                    // replaced it already.
                    let mut slot = slot_cap.borrow_mut();
                    if let Some(cur) = slot.as_ref() {
                        if cur.is_same_node(Some(clone_cap.as_ref())) {
                            *slot = None;
                        }
                    }
                });
            }
            *current.borrow_mut() = None;
        }
        prev_active.set(active);

        // ── mount the incoming branch ──
        let (body, fallback_template): (Option<IfBodyFn>, Option<&HtmlTemplateElement>) =
            match active {
                None => return,
                Some(0) => (branches[0].1, head_template.as_ref()),
                Some(i) if i < branches.len() => (
                    branches[i].1,
                    member_templates.get(i - 1).and_then(|t| t.as_ref()),
                ),
                Some(_) => (else_body, member_templates.last().and_then(|t| t.as_ref())),
            };

        let (clone_root, fragment_built) = match body {
            Some(f) => match f(scope_id, &proxy, ctx_parent_id) {
                Some(root) => (root, true),
                None => {
                    console::error_1(&JsValue::from_str(
                        "pp-if/pp-else: body fragment failed to materialise root",
                    ));
                    return;
                }
            },
            None => {
                let Some(root) = fallback_template.and_then(clone_template_body) else {
                    console::error_1(&JsValue::from_str(
                        "pp-if/pp-else: <template> body must contain exactly one element",
                    ));
                    return;
                };
                (root, false)
            }
        };

        // Pin the installing scope onto the clone (borrow mode —
        // removing the clone must not evict the owner's scope).
        bind_borrowed_scope_to(&clone_root, scope_id, &proxy);

        let inserted = match teleport_target.as_ref() {
            Some(target) => target.append_child(clone_root.as_ref()).is_ok(),
            None => anchor_node
                .parent_node()
                .map(|p| {
                    p.insert_before(clone_root.as_ref(), Some(&anchor_node))
                        .is_ok()
                })
                .unwrap_or(false),
        };
        if !inserted {
            return;
        }

        if teleport_target.is_some() {
            // Back-link teleport clones to the anchor's parent so
            // consumers can walk from the clone to the host, and
            // stash there so release_subtree removes the far-away
            // clone when the host subtree is torn down.
            let _ = js_sys::Reflect::set(
                clone_root.as_ref(),
                &teleport::TELEPORT_ORIGIN_KEY.into(),
                stash_el.as_ref(),
            );
            teleport::stash(&stash_el, &clone_root);
        }
        if fragment_built {
            mount::finalize_compiled_subtree(&clone_root);
        } else {
            // RFC-058 Phase 6.5 — bodies must come from macro
            // fragments; the legacy clone surfaces the
            // misclassification through the fail-fast counter.
            crate::templates_plan::record_plan_failure();
        }
        *current.borrow_mut() = Some(clone_root.clone());
        transition::enter_subtree(&clone_root, || {});
    });

    // Effect lifetime rides the anchor's PARENT element (the
    // comment can't carry the effects table) — torn down when the
    // enclosing subtree is.
    track_effect_on(&track_el, effect_id);
}

fn clone_template_body(template: &HtmlTemplateElement) -> Option<Element> {
    let fragment: web_sys::Node = template.content().clone_node_with_deep(true).ok()?;
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

// ─── RFC-094 Phase 3 — the pp-match controller ──────────────────

/// Extract `(tag, payload)` per serde's externally-tagged
/// encoding: a string IS its tag (covers plain `String` fields
/// too); an object with exactly one own key is `(key, value)`;
/// anything else has no tag and only `_` matches.
fn extract_tag(v: &JsValue) -> (Option<String>, JsValue) {
    if let Some(s) = v.as_string() {
        return (Some(s), JsValue::UNDEFINED);
    }
    if v.is_object() {
        let keys = js_sys::Object::keys(v.unchecked_ref::<js_sys::Object>());
        if keys.length() == 1 {
            let key = keys.get(0);
            if let Some(tag) = key.as_string() {
                let payload = js_sys::Reflect::get(v, &key).unwrap_or(JsValue::UNDEFINED);
                return (Some(tag), payload);
            }
        }
    }
    (None, JsValue::UNDEFINED)
}

/// One resolved `pp-case` arm.
pub struct MatchArm {
    /// Variant names; empty = `_` wildcard.
    pub tags: &'static [&'static str],
    pub bind_name: Option<&'static str>,
    pub body: Option<IfBodyFn>,
}

/// Install the `pp-match` controller. Same anchor contract as
/// the chain controller (comment swap, scope-root exception),
/// same exactly-one-clone invariant; the selector is tag
/// extraction + first matching arm, and `pp-let` arms mount
/// their body against a per-mount
/// [`PayloadScope`](crate::payload_scope::PayloadScope) whose value
/// updates in place on same-tag payload changes (no remount).
pub fn install_match(
    template: HtmlTemplateElement,
    scope_id: ScopeId,
    proxy: JsValue,
    evaluator: BranchEval,
    arms: Vec<MatchArm>,
    teleport_selector: Option<&str>,
) {
    let template_el: Element = template.clone().into();
    let teleport_selector = teleport_selector
        .map(str::to_string)
        .or_else(|| template_el.get_attribute("pp-teleport"));
    let teleport_target: Option<Element> = teleport_selector
        .as_deref()
        .and_then(teleport::resolve_target);
    let inject_parent_id_override = mount::inherited_ctx_parent_of(&template_el);
    let ctx_parent_id = inject_parent_id_override.unwrap_or(scope_id);

    let Some(parent_el) = template_el.parent_element() else {
        return;
    };
    let is_scope_root = mount::scope_id_of_element(&template_el).is_some();
    let (anchor_node, stash_el): (web_sys::Node, Element) = if is_scope_root {
        (template_el.clone().into(), template_el.clone())
    } else {
        let Some(doc) = template_el.owner_document() else {
            return;
        };
        let anchor = doc.create_comment("pp:match");
        let anchor_node: web_sys::Node = anchor.into();
        if parent_el
            .replace_child(&anchor_node, template_el.as_ref())
            .is_err()
        {
            return;
        }
        (anchor_node, parent_el.clone())
    };

    let current: Rc<RefCell<Option<Element>>> = Rc::new(RefCell::new(None));
    let prev_active: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
    let payload_scope: Rc<RefCell<Option<crate::scope::Scope>>> = Rc::new(RefCell::new(None));
    // Mid-leave cancel record — see install_cond. Carries the
    // arm's PayloadScope so a resumed pp-let arm keeps receiving
    // same-tag payload updates.
    #[allow(clippy::type_complexity)]
    let leaving: Rc<RefCell<Option<(Option<usize>, Element, Option<crate::scope::Scope>)>>> =
        Rc::new(RefCell::new(None));
    let track_el = stash_el.clone();

    let effect_id = effect(move || {
        let value = evaluator(&proxy);
        let (tag, payload) = extract_tag(&value);
        let active = arms.iter().position(|arm| {
            arm.tags.is_empty()
                || tag
                    .as_deref()
                    .map(|t| arm.tags.contains(&t))
                    .unwrap_or(false)
        });

        if active == prev_active.get() {
            // Same arm. Same-tag payload change: update the
            // payload scope's value in place and trigger its
            // ident — the body's own bindings re-render without
            // a remount (Solid's non-keyed Match semantics).
            if let Some(scope) = payload_scope.borrow().as_ref() {
                if let Some(state) = scope.typed::<crate::payload_scope::PayloadScope>() {
                    let ident = {
                        let mut st = state.borrow_mut();
                        st.value = if arms
                            .get(active.unwrap_or_default())
                            .map(|a| a.tags.is_empty())
                            .unwrap_or(false)
                        {
                            value.clone()
                        } else {
                            payload.clone()
                        };
                        st.ident.clone()
                    };
                    crate::reactive::trigger(scope.id, &ident);
                }
            }
            if let Some(clone) = current.borrow().as_ref() {
                if transition::is_subtree_leaving(clone) {
                    transition::enter_subtree(clone, || {});
                }
            }
            return;
        }

        // Mid-leave cancel — resume the same clone (and its
        // payload scope) when flipping back to a still-leaving arm.
        let resumable = {
            let mut l = leaving.borrow_mut();
            match l.take() {
                Some((idx, el, payload)) if idx == active && el.is_connected() => {
                    Some((el, payload))
                }
                other => {
                    *l = other;
                    None
                }
            }
        };
        if let Some((el, payload)) = resumable {
            transition::enter_subtree(&el, || {});
            *current.borrow_mut() = Some(el);
            *payload_scope.borrow_mut() = payload;
            prev_active.set(active);
            return;
        }

        // ── teardown the outgoing clone ──
        let existing = current.borrow().clone();
        if let Some(clone) = existing {
            if !transition::is_subtree_leaving(&clone) {
                let clone_cap = clone.clone();
                let slot_cap = current.clone();
                let stash_cap = stash_el.clone();
                let leaving_cap = leaving.clone();
                let teleported = teleport_target.is_some();
                *leaving.borrow_mut() = Some((
                    prev_active.get(),
                    clone.clone(),
                    payload_scope.borrow_mut().take(),
                ));
                transition::leave_subtree(&clone, move || {
                    if let Some(parent) = clone_cap.parent_node() {
                        let _ = parent.remove_child(&clone_cap);
                    }
                    if teleported {
                        teleport::clear_stash(&stash_cap, &clone_cap);
                    }
                    mount::release_subtree(clone_cap.as_ref());
                    {
                        let mut l = leaving_cap.borrow_mut();
                        if l.as_ref()
                            .map(|(_, el, _)| el.is_same_node(Some(clone_cap.as_ref())))
                            .unwrap_or(false)
                        {
                            *l = None;
                        }
                    }
                    let mut slot = slot_cap.borrow_mut();
                    if let Some(cur) = slot.as_ref() {
                        if cur.is_same_node(Some(clone_cap.as_ref())) {
                            *slot = None;
                        }
                    }
                });
            }
            *current.borrow_mut() = None;
        }
        *payload_scope.borrow_mut() = None;
        prev_active.set(active);

        // ── mount the incoming arm ──
        let Some(arm_idx) = active else { return };
        let arm = &arms[arm_idx];
        let Some(body) = arm.body else {
            // Unliftable case body — fail-fast empty, the
            // Phase 6.5 contract.
            crate::templates_plan::record_plan_failure();
            return;
        };

        // pp-let arms mount against a per-mount payload scope
        // (released with the clone — it is stamped OWNED, not
        // borrowed); bare arms bind straight to the owner.
        let (mount_scope_id, owned_payload) = match arm.bind_name {
            Some(name) => {
                let bound = if arm.tags.is_empty() {
                    value.clone()
                } else {
                    payload.clone()
                };
                let scope = crate::scope::Scope::new(Rc::new(RefCell::new(
                    crate::payload_scope::PayloadScope {
                        ident: name.to_string(),
                        value: bound,
                        parent_scope_id: scope_id,
                    },
                )));
                crate::context::set_parent(scope.id, ctx_parent_id);
                let id = scope.id;
                *payload_scope.borrow_mut() = Some(scope);
                (id, true)
            }
            None => (scope_id, false),
        };

        let Some(clone_root) = body(mount_scope_id, &proxy, ctx_parent_id) else {
            console::error_1(&JsValue::from_str(
                "pp-case: body fragment failed to materialise root",
            ));
            return;
        };
        if owned_payload {
            // Owned scope: released by release_subtree when the
            // clone goes (the for_-row pattern). Id-only stamp —
            // any dynamic need lazy-mints.
            mount::bind_scope_id_only(&clone_root, mount_scope_id);
            // The body fragment fn stamped the root BORROWED
            // (stamp_if_body_with's default); this scope is ours
            // and must die with the clone.
            mount::mark_scope_owned(&clone_root);
        } else {
            bind_borrowed_scope_to(&clone_root, scope_id, &proxy);
        }

        let inserted = match teleport_target.as_ref() {
            Some(target) => target.append_child(clone_root.as_ref()).is_ok(),
            None => anchor_node
                .parent_node()
                .map(|p| {
                    p.insert_before(clone_root.as_ref(), Some(&anchor_node))
                        .is_ok()
                })
                .unwrap_or(false),
        };
        if !inserted {
            return;
        }
        if teleport_target.is_some() {
            let _ = js_sys::Reflect::set(
                clone_root.as_ref(),
                &teleport::TELEPORT_ORIGIN_KEY.into(),
                stash_el.as_ref(),
            );
            teleport::stash(&stash_el, &clone_root);
        }
        mount::finalize_compiled_subtree(&clone_root);
        *current.borrow_mut() = Some(clone_root.clone());
        transition::enter_subtree(&clone_root, || {});
    });

    track_effect_on(&track_el, effect_id);
}
