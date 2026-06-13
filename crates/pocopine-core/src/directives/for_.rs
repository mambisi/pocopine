//! `pp-for="item in items"` — iterate an array, clone the host
//! `<template>`'s body once per item, bind each clone against a
//! [`crate::loop_scope::LoopScope`].
//!
//! Requires the host to be a `<template>` controller. HTML templates
//! clone their inert `content`; SVG templates are treated as
//! controller anchors whose prototype child is captured and cleared
//! during install (RFC 068). Clones are inserted as siblings before
//! the template.
//!
//! Two modes, controlled by the optional `pp-key` attribute:
//!
//! * **Naive (no `pp-key`)** — every reactive re-run tears down every
//!   prior clone and creates fresh ones. Simple, correct, loses any
//!   per-clone state. RFC-004 §7.1.
//! * **Keyed (`pp-key="<path>"`)** — each clone is tagged with a
//!   stable key derived from the item; on re-run, clones whose keys
//!   still appear get their `LoopScope` updated in place + their
//!   effects re-fired via `trigger_scope`. New keys get new clones;
//!   dropped keys get removed. See RFC-007.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use js_sys::{Array, Object, Reflect};
use smallvec::SmallVec;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, DocumentFragment, Element, HtmlTemplateElement, Node};

use crate::loop_scope::LoopScope;
use crate::mount::{self, bind_scope_to, track_effect_on};
use crate::path::resolve_path;
use crate::reactive::{effect, trigger_scope, ScopeId};
use crate::scope::Scope;

/// Controller source for `pp-for`.
///
/// HTML `<template pp-for>` exposes inert row content through
/// `HTMLTemplateElement::content`. SVG `<template pp-for>` does
/// not; browsers keep it as a foreign element with live children.
/// RFC 068 treats that SVG node as a controller anchor and stores
/// a cloned prototype before clearing the live children.
#[derive(Clone)]
pub struct ForTemplate {
    anchor: Element,
    html: Option<HtmlTemplateElement>,
    inline_prototype: Option<Element>,
}

impl ForTemplate {
    pub fn from_element(el: Element) -> Option<Self> {
        if let Ok(template) = el.clone().dyn_into::<HtmlTemplateElement>() {
            return Some(Self {
                anchor: el,
                html: Some(template),
                inline_prototype: None,
            });
        }

        if el.local_name() != "template" {
            return None;
        }

        let inline_prototype = el
            .first_element_child()
            .and_then(|child| child.clone_node_with_deep(true).ok())
            .and_then(|node| node.dyn_into::<Element>().ok());

        while let Some(child) = el.first_child() {
            let _ = el.remove_child(&child);
        }
        let _ = el.set_attribute("aria-hidden", "true");
        let _ = el.set_attribute("style", "display:none");

        Some(Self {
            anchor: el,
            html: None,
            inline_prototype,
        })
    }

    fn anchor(&self) -> Element {
        self.anchor.clone()
    }

    /// RFC-095 W4 — the live prototype element the mutation
    /// channel clones from JS-side (no per-row wasm crossing).
    fn prototype(&self) -> Option<Element> {
        if let Some(template) = &self.html {
            return template.content().first_element_child();
        }
        self.inline_prototype.clone()
    }

    fn clone_body(&self) -> Option<Element> {
        if let Some(template) = &self.html {
            return clone_template_body(template);
        }
        self.inline_prototype
            .as_ref()?
            .clone_node_with_deep(true)
            .ok()?
            .dyn_into::<Element>()
            .ok()
    }
}

/// Return the element child whose layout box represents the
/// custom-element's visible bounds. Custom tags themselves are
/// `display: inline` by default and `getBoundingClientRect` returns
/// zero for them, so FLIP needs to measure the rendered root one
/// level deeper.
fn first_layout_child(el: &Element) -> Option<Element> {
    let children = el.children();
    if children.length() == 0 {
        return None;
    }
    children.item(0)
}

fn retract_from_prior(prior: &Rc<RefCell<Vec<PrevItem>>>, key: &RowKey) {
    let mut p = prior.borrow_mut();
    // RFC-101 P3 — value equality, not `Rc::ptr_eq`: `RowKey::Int`
    // has no pointer identity. The retract callback is handed a clone
    // of the leaver's own key (`key_for_retract`), so `==` matches the
    // same logical row; this is strictly more correct than pointer
    // identity and only runs on the (cold) leave path.
    p.retain(|item| !(item.key == *key && item.leaving));
}

/// Fire enter-subtree on every `el` in `clones`, spacing them by
/// `stagger_ms * index`. `stagger_ms == 0` fires every clone
/// simultaneously with no per-element timer overhead; the non-zero
/// path routes through `transition::enter_subtrees_sequenced` so
/// clones snap to their start state at insertion time (no flash of
/// natural state while waiting for a delayed `enter` call).
fn fire_staggered_enter(clones: &[Element], stagger_ms: u32) {
    if stagger_ms == 0 {
        for el in clones {
            crate::directives::transition::enter_subtree(el, || {});
        }
        return;
    }
    crate::directives::transition::enter_subtrees_sequenced(clones, stagger_ms);
}

fn remove_or_leave(root: &Element) {
    if !crate::directives::transition::has_transition_in_subtree(root) {
        if let Some(parent) = root.parent_node() {
            let _ = parent.remove_child(root);
        }
        return;
    }
    let root_cap = root.clone();
    crate::directives::transition::leave_subtree(root, move || {
        if let Some(parent) = root_cap.parent_node() {
            let _ = parent.remove_child(&root_cap);
        }
    });
}

/// Predicate for the bulk-clear path. `replace_children_with_node_1`
/// removes every child of `parent_el` (text + comment included)
/// before reinstating the anchor. We only take the fast path
/// when:
///
/// * The parent has exactly `pool.len()` element children (plus one
///   when the anchor is itself an element — the scope-root case),
///   and every pool clone plus the anchor is a direct child of that
///   parent — no user-authored element sibling can fit in that count.
/// * Every non-element child is a whitespace-only `Text` node
///   (typical formatting indentation between rows) or the
///   controller's own `<!--pp:for-->` comment anchor.
///
/// Foreign comments, non-whitespace text, and unrecognised element
/// siblings all fall through to the per-row remove loop.
fn bulk_clear_safe<'a>(
    parent_el: &Element,
    entries: impl IntoIterator<Item = &'a PrevItem>,
    anchor: &Node,
    pool_count: usize,
) -> bool {
    let parent_node: &Node = parent_el.as_ref();
    let anchor_is_element = anchor.node_type() == Node::ELEMENT_NODE;
    if parent_el.child_element_count() as usize != pool_count + usize::from(anchor_is_element) {
        return false;
    }
    let anchor_parent_ok = anchor
        .parent_node()
        .as_ref()
        .is_some_and(|parent| parent.is_same_node(Some(parent_node)));
    if !anchor_parent_ok {
        return false;
    }
    for entry in entries {
        let row_parent_ok = entry
            .element
            .parent_node()
            .as_ref()
            .is_some_and(|parent| parent.is_same_node(Some(parent_node)));
        if !row_parent_ok {
            return false;
        }
    }

    let nodes = parent_el.child_nodes();
    for i in 0..nodes.length() {
        let Some(node) = nodes.item(i) else {
            return false;
        };
        if node.is_same_node(Some(anchor)) {
            continue;
        }
        match node.node_type() {
            Node::ELEMENT_NODE => {}
            Node::TEXT_NODE => {
                let text = node.text_content().unwrap_or_default();
                if !text.chars().all(|c| c.is_whitespace()) {
                    return false;
                }
            }
            // Foreign comment, CDATA, processing instruction, etc.
            _ => return false,
        }
    }
    true
}

fn bulk_clear_compiled(parent_el: &Element, entries: &[PrevItem], anchor: &Node) -> bool {
    let pool_count = entries.len();
    // Compiled row plans are emitted only for the macro envelope
    // that rejects transition attributes. That makes a per-row
    // transition scan redundant here; generic rows still use the
    // transition-aware removal path.
    if pool_count == 0 || !bulk_clear_safe(parent_el, entries.iter(), anchor, pool_count) {
        return false;
    }

    let scope_ids: Vec<ScopeId> = entries.iter().map(|entry| entry.scope_id).collect();
    // RFC-058 Phase 6.5 — MutationObserver-driven release-skip
    // marker is gone with the mount. Synchronous bulk teardown
    // does the work directly below.
    crate::directives::for_plan::unmount_rows_bulk(&scope_ids);
    Scope::remove_compiled_rows(&scope_ids);
    parent_el.replace_children_with_node_1(anchor);
    true
}

fn flip_target_for_entry(entry: &PrevItem) -> Option<Element> {
    // RFC-058 Phase 6.4 — `clone_template_body` returns an
    // Element whose `parent_node` is the cloned
    // `DocumentFragment` it was extracted from, NOT `None`.
    // The original `parent_node().is_none()` guard intended to
    // skip new (not-yet-inserted) entries was therefore a
    // no-op for cloneNode-based clones, and the snapshot
    // captured `(0, 0)` because the fragment is detached from
    // the live document. flip_batch then animated the entry
    // from origin to its final position — visible as a chip
    // "flying in from the document upper-left" on every add.
    // `is_connected` is true only when the element is part of
    // the live document tree, which is the actual signal
    // pp-for needs.
    if !entry.element.is_connected()
        || entry.element.get_attribute("data-pp-animate").as_deref() != Some("flip")
    {
        return None;
    }
    Some(first_layout_child(&entry.element).unwrap_or_else(|| entry.element.clone()))
}

fn lift_leaver_out_of_layout(entry: &PrevItem) {
    let Some(target) = flip_target_for_entry(entry) else {
        return;
    };
    let rect = target.get_bounding_client_rect();
    let Some(html) = target.dyn_ref::<web_sys::HtmlElement>() else {
        return;
    };
    let style = html.style();
    let _ = style.set_property("position", "fixed");
    let _ = style.set_property("left", &format!("{}px", rect.left()));
    let _ = style.set_property("top", &format!("{}px", rect.top()));
    let _ = style.set_property("width", &format!("{}px", rect.width()));
    let _ = style.set_property("height", &format!("{}px", rect.height()));
    let _ = style.set_property("margin", "0");
    let _ = style.set_property("z-index", "1");
    let _ = style.set_property("pointer-events", "none");
}

fn restore_leaver_layout(entry: &PrevItem) {
    let Some(target) = flip_target_for_entry(entry) else {
        return;
    };
    let Some(html) = target.dyn_ref::<web_sys::HtmlElement>() else {
        return;
    };
    let style = html.style();
    for prop in [
        "position",
        "left",
        "top",
        "width",
        "height",
        "margin",
        "z-index",
        "pointer-events",
    ] {
        let _ = style.remove_property(prop);
    }
}

/// Compiled-path entry point. Skips the `<template>` cast +
/// `pp-for` parse + `pp-key` / `pp-stagger` attribute reads —
/// the macro provides everything pre-resolved.
///
/// Called by the compiled plan installer for every `StaticForPlan`
/// entry the classifier emitted. RFC-058 Phase 4.2 — extracted
/// so the runtime mount dispatch path and the plan applier
/// share one keyed/naive selection body. Coexists with the
/// RFC-054 row-plan registry: `lookup_for_template` reads
/// `data-pp-row-plan` from the template element, which the
/// template-plan classifier bakes in via the §6.2 layering
/// path.
#[allow(clippy::too_many_arguments)]
pub fn install(
    template: ForTemplate,
    parent_proxy: JsValue,
    parent_scope_id: ScopeId,
    item_name: &'static str,
    items_expr: &'static str,
    key_expr: Option<&'static str>,
    stagger_ms: u32,
    body: Option<crate::directives::for_plan::ForBodyFn>,
) {
    let template_el = template.anchor();
    let track_anchor = template_el.clone();
    // RFC-027 inject chain — when this `pp-for` was authored as
    // slot content, the slot materialiser stamped
    // `CTX_PARENT_KEY` on the `<template>` element pointing at
    // the slot OWNER (the component whose template contains the
    // `<slot>`). The LoopScope's inject parent should chain
    // through the owner so descendants reach the owner's
    // provided contexts (e.g. the `ROOT` context that
    // `pine-tags-input-root.on_setup` provides). Without this,
    // a `pp-for` of `<pine-tags-input-item>`s inside the root's
    // slot has its LoopScope chain to the SLOT AUTHOR (the page
    // component) instead of through the root, and
    // `<pine-tags-input-item-delete>::ROOT.inject()` returns
    // `None`. The `parent_scope_id` arg stays the AUTHOR scope
    // so directive resolution (`tag` etc.) keeps reading from
    // the caller, per RFC-011.
    // Walk ancestors (not just the template itself) — when the
    // pp-for is wrapped inside slot content like
    // `<div><template pp-for>`, the slot materialiser only stamps
    // the top-level slot child (the wrapping `<div>`), not the
    // nested `<template>`. Reading just the template would miss
    // the owner stamp and chain to the slot author.
    let inject_parent_id =
        crate::mount::inherited_ctx_parent_of(&template_el).unwrap_or(parent_scope_id);
    // RFC 054 — opportunistic compiled-row-plan fast path, looked
    // up once per `pp-for` site (NOT per row). MUST happen before
    // the anchor swap below: `lookup_for_template` namespaces the
    // `data-pp-row-plan` id by walking `parent_element()` to the
    // nearest component stamp, and a detached template has no
    // ancestry.
    let row_plan: Option<Rc<crate::directives::for_plan::CompiledRowPlan>> =
        crate::directives::for_plan::lookup_for_template(&template_el);
    // RFC-094 Phase 4 — swap the template for a `<!--pp:for-->`
    // comment anchor. Same contract as the cond/match controllers:
    // the comment holds the clones' insertion position with no
    // element-indexed footprint (`:nth-child` et al. see only live
    // rows), and a template that IS the component's scope-carrying
    // root stays put (removing it would orphan the scope stamp).
    // The detached template stays alive on `ForTemplate` for
    // `clone_body` and the row-plan attribute lookup.
    let is_scope_root = mount::scope_id_of_element(&template_el).is_some();
    let (anchor, track_el): (Node, Element) = if is_scope_root {
        (template_el.clone().into(), track_anchor)
    } else {
        match (template_el.owner_document(), template_el.parent_element()) {
            (Some(doc), Some(parent_el)) => {
                let comment_node: Node = doc.create_comment("pp:for").into();
                if parent_el
                    .replace_child(&comment_node, template_el.as_ref())
                    .is_ok()
                {
                    (comment_node, parent_el)
                } else {
                    (template_el.clone().into(), track_anchor)
                }
            }
            _ => (template_el.clone().into(), track_anchor),
        }
    };
    // `pp-stagger="<ms>"` on the template spreads the enter
    // animation of newly-inserted clones across time — `i * stagger`
    // ms delay per clone, in insertion order. Keeps a batch mount
    // of a big list (stress fixture dropping 500 tags at once)
    // from firing every scale-in simultaneously.
    let effect_id = match key_expr {
        Some(key) if !key.trim().is_empty() => run_keyed(
            item_name,
            items_expr,
            key,
            parent_proxy,
            parent_scope_id,
            inject_parent_id,
            template,
            anchor,
            stagger_ms,
            body,
            row_plan,
        ),
        _ => run_naive(
            item_name,
            items_expr,
            parent_proxy,
            parent_scope_id,
            inject_parent_id,
            template,
            anchor,
            stagger_ms,
            body,
        ),
    };

    track_effect_on(&track_el, effect_id);
}

/// Whole-rebuild iteration (no `pp-key`). Keeps the original
/// RFC-004 semantics.
#[allow(clippy::too_many_arguments)]
fn run_naive(
    item_name: &'static str,
    items_expr: &'static str,
    parent_proxy: JsValue,
    parent_scope_id: ScopeId,
    inject_parent_id: ScopeId,
    template: ForTemplate,
    anchor: Node,
    stagger_ms: u32,
    body: Option<crate::directives::for_plan::ForBodyFn>,
) -> crate::reactive::EffectId {
    let item_name: Rc<str> = item_name.into();
    let prior: Rc<RefCell<Vec<Element>>> = Rc::new(RefCell::new(Vec::new()));
    // RFC-095 W1 — items reads resolve Rust-side (track + cache)
    // unless the path roots at a `$`-name (e.g. `$store.items`),
    // which falls back to the proxy.
    let items_reader = crate::scope::scoped_root_reader(parent_scope_id);

    effect(move || {
        let items_js =
            crate::path::resolve_path_with(&parent_proxy, items_reader.as_ref(), items_expr);
        let arr: Array = items_js
            .dyn_into::<Array>()
            .unwrap_or_else(|_| Array::new());
        let total = arr.length() as usize;

        {
            let mut prior = prior.borrow_mut();
            for el in prior.drain(..) {
                remove_or_leave(&el);
            }
        }
        if total == 0 {
            return;
        }

        let Some(parent_node) = anchor.parent_node() else {
            return;
        };

        let mut fresh: Vec<Element> = Vec::with_capacity(total);
        for i in 0..total {
            let item = arr.get(i as u32);
            let loop_state = LoopScope {
                item_name: Rc::clone(&item_name),
                item,
                index: i,
                total,
                parent_scope_id,
            };
            let scope = Scope::new(Rc::new(RefCell::new(loop_state)));
            crate::context::set_parent(scope.id, inject_parent_id);
            let proxy = scope.into_proxy();

            // RFC-058 Phase 4.2c — when the macro emitted a body
            // fragment for this pp-for site, build the row root
            // through the fragment (which stamps cleaned HTML +
            // installs generated entries against the row's
            // LoopScope via the Phase 1 helpers). We still run
            // the mount after insertion so preserved fallback
            // directives in nested custom-component templates bind.
            let (clone_root, fragment_built) = match body {
                Some(f) => match f(scope.id, &proxy, scope.id) {
                    Some(root) => (root, true),
                    None => {
                        console::error_1(&JsValue::from_str(
                            "pp-for: row body fragment failed to materialise root",
                        ));
                        break;
                    }
                },
                None => match template.clone_body() {
                    Some(root) => (root, false),
                    None => {
                        console::error_1(&JsValue::from_str(
                            "pp-for: <template> body must contain exactly one element",
                        ));
                        break;
                    }
                },
            };
            bind_scope_to(&clone_root, scope.id, &proxy);

            if parent_node
                .insert_before(clone_root.as_ref(), Some(&anchor))
                .is_ok()
            {
                if fragment_built {
                    mount::finalize_compiled_subtree(&clone_root);
                } else {
                    // RFC-058 Phase 6.5 — `body_fn = None` means
                    // the macro couldn't lift this row's body
                    // (typical of pp-for inside user-authored slot
                    // content, where the macro doesn't see the
                    // runtime-captured slot template). Stamp
                    // `CTX_PARENT_KEY` = the LoopScope so any
                    // custom tag inside the cloned body chains
                    // its inject parent through this row's scope
                    // — matches what `stamp_if_body` does for the
                    // body_fn = Some path. Then drive registered-
                    // tag discovery + lifecycle. Native `pp-*`
                    // directives on the clone stay inert — that's
                    // the documented compiled-only contract.
                    let ctx_key = wasm_bindgen::JsValue::from_str(mount::CTX_PARENT_KEY);
                    let ctx_val = wasm_bindgen::JsValue::from_f64(scope.id.0 as f64);
                    let _ = js_sys::Reflect::set(clone_root.as_ref(), &ctx_key, &ctx_val);
                    mount::finalize_compiled_subtree(&clone_root);
                }
                fresh.push(clone_root);
            }
        }

        fire_staggered_enter(&fresh, stagger_ms);

        *prior.borrow_mut() = fresh;
    })
}

/// One previously-rendered clone. `loop_state` lets us mutate the
/// `LoopScope` in place on reuse without serializing through JS.
///
/// The key is shared between `PrevItem`, the pool lookup, and the
/// dedup-tracking set via [`RowKey`] — integer keys (the common
/// case) cost zero heap allocations; the dup-set insert clones an
/// `i64`, not a fresh `String`.
///
/// `leaving` is true while the clone is mid-leave (its transition
/// is playing; the remove-from-DOM callback hasn't fired). We keep
/// such entries in `prior` so a rapid re-mount whose items contain
/// the same key can reverse the leave and reuse the clone instead
/// of spawning a fresh one next to the still-leaving original.
struct PrevItem {
    element: Element,
    scope_id: ScopeId,
    loop_state: Rc<RefCell<LoopScope>>,
    key: RowKey,
    item_value: JsValue,
    item_sig: String,
    leaving: bool,
}

/// One minted-but-unmounted channel row: scope id, pool key,
/// item value, and the row's LoopScope.
type ChannelRowMeta = (ScopeId, RowKey, JsValue, Rc<RefCell<LoopScope>>);

/// RFC-095 W4 — mount `pairs.len()` fresh rows (indices
/// `start..start+pairs.len()` of `arr`) through the mutation
/// channel: scopes mint Rust-side, ONE interpreter crossing does
/// clone + scope stamp + item-rooted binding writes + fragment
/// insert before `anchor`, and the returned flat handle array
/// feeds both the reconcile pool entries and the RowInstance
/// registration. Caller owns key resolution + dedup (`pairs`).
#[allow(clippy::too_many_arguments)]
fn channel_mount_range(
    plan: &Rc<crate::directives::for_plan::CompiledRowPlan>,
    slot: u32,
    proto: &Element,
    anchor: &Node,
    arr: &Array,
    start: usize,
    total: usize,
    pairs: Vec<(RowKey, JsValue)>,
    item_name: &Rc<str>,
    parent_scope_id: ScopeId,
    inject_parent_id: ScopeId,
    stagger_ms: u32,
) -> Option<Vec<PrevItem>> {
    let count = pairs.len();
    let mut ids: Vec<f64> = Vec::with_capacity(count);
    let mut metas: Vec<ChannelRowMeta> = Vec::with_capacity(count);
    for (off, (key, item)) in pairs.into_iter().enumerate() {
        let loop_rc = Rc::new(RefCell::new(LoopScope {
            item_name: Rc::clone(item_name),
            item: item.clone(),
            index: start + off,
            total,
            parent_scope_id,
        }));
        let scope = Scope::new(loop_rc.clone());
        crate::context::set_parent(scope.id, inject_parent_id);
        ids.push(scope.id.0 as f64);
        metas.push((scope.id, key, item, loop_rc));
    }

    let dom_insert_start = crate::profiler::mount::start();
    let parent_vals = crate::directives::for_plan::channel_parent_vals(slot, parent_scope_id);
    let out = crate::mutation_channel::mount_rows(
        slot,
        proto,
        anchor.as_ref(),
        arr.as_ref(),
        start as u32,
        count as u32,
        total as u32,
        &ids,
        &parent_vals,
    );
    crate::profiler::mount::record_dom_insertion(dom_insert_start);
    // `null` = the interpreter refused (unresolvable node path —
    // checked against the prototype before any DOM mutation).
    // Release the minted scopes and let the caller fall back to
    // the direct mount path, mirroring its warn-and-degrade.
    if out.is_null() {
        web_sys::console::warn_1(&JsValue::from_str(
            "pp-for channel: plan node path did not resolve; using direct mount",
        ));
        for (scope_id, ..) in &metas {
            Scope::remove(*scope_id);
        }
        return None;
    }
    let out: Array = out.unchecked_into();

    let b = plan.bindings.len();
    let l = plan.listeners.len();
    let stride = 1 + b + l;
    let mut prev_items: Vec<PrevItem> = Vec::with_capacity(count);
    let mut channel_rows: Vec<crate::directives::for_plan::ChannelRow> = Vec::with_capacity(count);
    let mut roots: Vec<Element> = Vec::with_capacity(count);
    for (r, (scope_id, key, item, loop_rc)) in metas.into_iter().enumerate() {
        let base = (r * stride) as u32;
        let root: Element = out.get(base).unchecked_into();
        let binding_nodes: SmallVec<[Element; 4]> = (0..b)
            .map(|j| out.get(base + 1 + j as u32).unchecked_into())
            .collect();
        let listener_nodes: SmallVec<[Element; 2]> = (0..l)
            .map(|j| out.get(base + 1 + (b + j) as u32).unchecked_into())
            .collect();
        channel_rows.push(crate::directives::for_plan::ChannelRow {
            scope_id,
            binding_nodes,
            listener_nodes,
            loop_state: loop_rc.clone(),
        });
        roots.push(root.clone());
        prev_items.push(PrevItem {
            element: root,
            scope_id,
            loop_state: loop_rc,
            key,
            item_value: item,
            item_sig: String::new(),
            leaving: false,
        });
    }
    crate::directives::for_plan::register_channel_rows(plan, channel_rows, parent_scope_id);
    // Every clone shares the prototype's static attributes, and
    // enter transitions key entirely on attributes — so ONE
    // prototype check replaces N per-row `enter_subtree` walks.
    // (`pp-stagger` is row-plan-ineligible, so stagger_ms > 0
    // can't normally reach here; the check keeps it safe anyway.)
    if stagger_ms > 0 || crate::directives::transition::has_transition_in_subtree(proto) {
        fire_staggered_enter(&roots, stagger_ms);
    }
    Some(prev_items)
}

#[allow(clippy::too_many_arguments)]
fn create_keyed_prev_item(
    item_name: &Rc<str>,
    item: JsValue,
    item_sig: String,
    index: usize,
    total: usize,
    parent_scope_id: ScopeId,
    inject_parent_id: ScopeId,
    template: &ForTemplate,
    key: RowKey,
    body: Option<crate::directives::for_plan::ForBodyFn>,
    elide_proxy: bool,
) -> Option<PrevItem> {
    let loop_rc = Rc::new(RefCell::new(LoopScope {
        item_name: Rc::clone(item_name),
        item: item.clone(),
        index,
        total,
        parent_scope_id,
    }));
    let scope = Scope::new(loop_rc.clone());
    crate::context::set_parent(scope.id, inject_parent_id);

    let clone_start = crate::profiler::mount::start();
    let clone_root = if let Some(body_fn) = body {
        let proxy = scope.into_proxy();
        match body_fn(scope.id, &proxy, scope.id) {
            Some(root) => {
                bind_scope_to(&root, scope.id, &proxy);
                Some(root)
            }
            None => {
                console::error_1(&JsValue::from_str(
                    "pp-for: row body fragment failed to materialise root",
                ));
                Scope::remove(scope.id);
                None
            }
        }
    } else {
        None
    };
    let clone_root = match clone_root {
        Some(root) => root,
        None => {
            let Some(root) = template.clone_body() else {
                console::error_1(&JsValue::from_str(
                    "pp-for: <template> body must contain exactly one element",
                ));
                Scope::remove(scope.id);
                return None;
            };
            if elide_proxy {
                mount::bind_scope_id_only(&root, scope.id);
            } else {
                let proxy = scope.into_proxy();
                bind_scope_to(&root, scope.id, &proxy);
            }
            let ctx_key = wasm_bindgen::JsValue::from_str(mount::CTX_PARENT_KEY);
            let ctx_val = wasm_bindgen::JsValue::from_f64(scope.id.0 as f64);
            let _ = js_sys::Reflect::set(root.as_ref(), &ctx_key, &ctx_val);
            root
        }
    };
    crate::profiler::mount::record_clone_template_body(clone_start);

    Some(PrevItem {
        element: clone_root,
        scope_id: scope.id,
        loop_state: loop_rc,
        key,
        item_value: item,
        item_sig,
        leaving: false,
    })
}

fn item_signature(v: &JsValue) -> String {
    if v.is_undefined() {
        return "u:".into();
    }
    if v.is_null() {
        return "n:".into();
    }
    if let Some(s) = v.as_string() {
        return format!("s:{s}");
    }
    if let Some(n) = v.as_f64() {
        return format!("f:{n}");
    }
    if let Some(b) = v.as_bool() {
        return if b { "b:1".into() } else { "b:0".into() };
    }
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .map(|s| format!("j:{s}"))
        .unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
fn try_append_fast_path(
    arr: &Array,
    total: usize,
    mut old_prior: Vec<PrevItem>,
    seen_cell: &Rc<RefCell<HashSet<RowKey>>>,
    key_resolver: &KeyResolver,
    item_name: &Rc<str>,
    parent_proxy: &JsValue,
    parent_scope_id: ScopeId,
    inject_parent_id: ScopeId,
    template: &ForTemplate,
    anchor: &Node,
    parent_node: &Node,
    body: Option<crate::directives::for_plan::ForBodyFn>,
    elide_proxy: bool,
    row_plan: Option<&Rc<crate::directives::for_plan::CompiledRowPlan>>,
    compiled_bindings_depend_on_position: bool,
    stagger_ms: u32,
) -> Result<Vec<PrevItem>, Vec<PrevItem>> {
    let old_len = old_prior.len();
    if old_len == 0 || total <= old_len {
        return Err(old_prior);
    }
    for (i, entry) in old_prior.iter().enumerate() {
        if entry.leaving || !Object::is(&entry.item_value, &arr.get(i as u32)) {
            return Err(old_prior);
        }
    }

    let row_iter_start = crate::profiler::reconcile::start();
    if compiled_bindings_depend_on_position {
        for entry in &old_prior {
            {
                let mut st = entry.loop_state.borrow_mut();
                st.total = total;
            }
            if !crate::directives::for_plan::reuse_row_compiled(entry.scope_id) {
                trigger_scope(entry.scope_id);
            }
        }
    } else {
        for entry in &old_prior {
            entry.loop_state.borrow_mut().total = total;
            trigger_scope(entry.scope_id);
        }
    }

    let mut seen = seen_cell.borrow_mut();
    seen.clear();
    let seen_cap = seen.capacity();
    if seen_cap < total {
        seen.reserve(total - seen_cap);
    }
    for entry in &old_prior {
        seen.insert(entry.key.clone());
    }

    // RFC-095 W4 — append the new suffix through the mutation
    // channel when eligible: same key dedup, one crossing for the
    // whole batch instead of per-row clone + mount.
    if elide_proxy && crate::mutation_channel::enabled() {
        if let (Some(plan), Some(proto)) = (row_plan, template.prototype()) {
            if let Some(slot) = crate::directives::for_plan::channel_slot(plan) {
                let mut pairs: Vec<(RowKey, JsValue)> = Vec::with_capacity(total - old_len);
                for i in old_len..total {
                    let item = arr.get(i as u32);
                    let key_val = key_resolver.resolve(&item, i, parent_proxy);
                    let key = dedup_row_key(&key_val, &mut seen, i);
                    pairs.push((key, item));
                }
                crate::profiler::reconcile::record_row_iter(row_iter_start);
                let reorder_start = crate::profiler::reconcile::start();
                let appended = channel_mount_range(
                    plan,
                    slot,
                    &proto,
                    anchor,
                    arr,
                    old_len,
                    total,
                    pairs,
                    item_name,
                    parent_scope_id,
                    inject_parent_id,
                    stagger_ms,
                );
                crate::profiler::reconcile::record_reorder(reorder_start);
                if let Some(appended) = appended {
                    drop(seen);
                    old_prior.extend(appended);
                    return Ok(old_prior);
                }
                // Interpreter refused — reset dedup to the prior
                // keys and fall through to the direct append loop.
                seen.clear();
                for entry in &old_prior {
                    seen.insert(entry.key.clone());
                }
            }
        }
    }

    let mut appended: Vec<PrevItem> = Vec::with_capacity(total - old_len);
    let create_body = if row_plan.is_none() { body } else { None };
    for i in old_len..total {
        let item = arr.get(i as u32);
        let key_val = key_resolver.resolve(&item, i, parent_proxy);
        let key = dedup_row_key(&key_val, &mut seen, i);
        let Some(entry) = create_keyed_prev_item(
            item_name,
            item,
            String::new(),
            i,
            total,
            parent_scope_id,
            inject_parent_id,
            template,
            key,
            create_body,
            elide_proxy,
        ) else {
            return Err(old_prior);
        };
        appended.push(entry);
    }
    drop(seen);
    crate::profiler::reconcile::record_row_iter(row_iter_start);

    let reorder_start = crate::profiler::reconcile::start();
    let doc = anchor
        .owner_document()
        .expect("anchor should belong to a document");
    let fragment: DocumentFragment = doc.create_document_fragment();
    let dom_insert_start = crate::profiler::mount::start();
    for entry in &appended {
        let _ = fragment.append_child(entry.element.as_ref());
    }
    let _ = parent_node.insert_before(fragment.as_ref(), Some(anchor));
    crate::profiler::mount::record_dom_insertion(dom_insert_start);

    for entry in &appended {
        if let Some(plan) = row_plan {
            if let Some(sid) = mount::scope_id_of_element(&entry.element) {
                let proxy_for_mount = if elide_proxy {
                    None
                } else {
                    crate::mount::scope_of_element(&entry.element).map(|(_, p)| p)
                };
                crate::directives::for_plan::mount_row_compiled(
                    plan,
                    &entry.element,
                    sid,
                    proxy_for_mount.as_ref(),
                );
                continue;
            }
        }
        crate::profiler::mount::record_generic_row_mounted();
        mount::finalize_compiled_subtree(&entry.element);
    }
    let entered = appended
        .iter()
        .map(|entry| entry.element.clone())
        .collect::<Vec<_>>();
    fire_staggered_enter(&entered, stagger_ms);
    crate::profiler::reconcile::record_reorder(reorder_start);

    old_prior.extend(appended);
    Ok(old_prior)
}

#[allow(clippy::too_many_arguments)]
fn try_prepend_fast_path(
    arr: &Array,
    total: usize,
    old_prior: Vec<PrevItem>,
    seen_cell: &Rc<RefCell<HashSet<RowKey>>>,
    key_resolver: &KeyResolver,
    item_name: &Rc<str>,
    parent_proxy: &JsValue,
    parent_scope_id: ScopeId,
    inject_parent_id: ScopeId,
    template: &ForTemplate,
    parent_node: &Node,
    body: Option<crate::directives::for_plan::ForBodyFn>,
    elide_proxy: bool,
    row_plan: Option<&Rc<crate::directives::for_plan::CompiledRowPlan>>,
    compiled_bindings_depend_on_position: bool,
    stagger_ms: u32,
) -> Result<Vec<PrevItem>, Vec<PrevItem>> {
    let old_len = old_prior.len();
    if old_len == 0 || total <= old_len {
        return Err(old_prior);
    }
    let added = total - old_len;
    for (i, entry) in old_prior.iter().enumerate() {
        if entry.leaving || !Object::is(&entry.item_value, &arr.get((i + added) as u32)) {
            return Err(old_prior);
        }
    }

    let row_iter_start = crate::profiler::reconcile::start();
    if compiled_bindings_depend_on_position {
        for (i, entry) in old_prior.iter().enumerate() {
            {
                let mut st = entry.loop_state.borrow_mut();
                st.index = i + added;
                st.total = total;
            }
            if !crate::directives::for_plan::reuse_row_compiled(entry.scope_id) {
                trigger_scope(entry.scope_id);
            }
        }
    } else {
        for (i, entry) in old_prior.iter().enumerate() {
            let mut st = entry.loop_state.borrow_mut();
            st.index = i + added;
            st.total = total;
            drop(st);
            trigger_scope(entry.scope_id);
        }
    }

    let mut seen = seen_cell.borrow_mut();
    seen.clear();
    let seen_cap = seen.capacity();
    if seen_cap < total {
        seen.reserve(total - seen_cap);
    }
    for entry in &old_prior {
        seen.insert(entry.key.clone());
    }

    let create_body = if row_plan.is_none() { body } else { None };
    let mut prepended: Vec<PrevItem> = Vec::with_capacity(added);
    for i in 0..added {
        let item = arr.get(i as u32);
        let key_val = key_resolver.resolve(&item, i, parent_proxy);
        let key = dedup_row_key(&key_val, &mut seen, i);
        let Some(entry) = create_keyed_prev_item(
            item_name,
            item,
            String::new(),
            i,
            total,
            parent_scope_id,
            inject_parent_id,
            template,
            key,
            create_body,
            elide_proxy,
        ) else {
            return Err(old_prior);
        };
        prepended.push(entry);
    }
    drop(seen);
    crate::profiler::reconcile::record_row_iter(row_iter_start);

    let reorder_start = crate::profiler::reconcile::start();
    let doc = old_prior
        .first()
        .and_then(|entry| entry.element.owner_document())
        .expect("existing row should belong to a document");
    let fragment: DocumentFragment = doc.create_document_fragment();
    let dom_insert_start = crate::profiler::mount::start();
    for entry in &prepended {
        let _ = fragment.append_child(entry.element.as_ref());
    }
    let anchor = old_prior
        .first()
        .map(|entry| entry.element.as_ref() as &Node);
    let _ = parent_node.insert_before(fragment.as_ref(), anchor);
    crate::profiler::mount::record_dom_insertion(dom_insert_start);

    for entry in &prepended {
        if let Some(plan) = row_plan {
            if let Some(sid) = mount::scope_id_of_element(&entry.element) {
                let proxy_for_mount = if elide_proxy {
                    None
                } else {
                    crate::mount::scope_of_element(&entry.element).map(|(_, p)| p)
                };
                crate::directives::for_plan::mount_row_compiled(
                    plan,
                    &entry.element,
                    sid,
                    proxy_for_mount.as_ref(),
                );
                continue;
            }
        }
        crate::profiler::mount::record_generic_row_mounted();
        mount::finalize_compiled_subtree(&entry.element);
    }
    let entered = prepended
        .iter()
        .map(|entry| entry.element.clone())
        .collect::<Vec<_>>();
    fire_staggered_enter(&entered, stagger_ms);
    crate::profiler::reconcile::record_reorder(reorder_start);

    prepended.extend(old_prior);
    Ok(prepended)
}

fn update_reused_entry(
    entry: &mut PrevItem,
    index: usize,
    total: usize,
    compiled_bindings_depend_on_position: bool,
) {
    let position_changed = {
        let st = entry.loop_state.borrow();
        st.index != index || st.total != total
    };
    if !position_changed {
        return;
    }
    {
        let mut st = entry.loop_state.borrow_mut();
        st.index = index;
        st.total = total;
    }
    if compiled_bindings_depend_on_position
        && !crate::directives::for_plan::reuse_row_compiled(entry.scope_id)
    {
        trigger_scope(entry.scope_id);
    }
}

#[allow(clippy::too_many_arguments)]
fn try_single_remove_fast_path(
    arr: &Array,
    total: usize,
    old_prior: Vec<PrevItem>,
    row_plan: Option<&Rc<crate::directives::for_plan::CompiledRowPlan>>,
    compiled_bindings_depend_on_position: bool,
) -> Result<Vec<PrevItem>, Vec<PrevItem>> {
    let old_len = old_prior.len();
    if old_len == 0 || old_len != total + 1 {
        return Err(old_prior);
    }
    if row_plan.is_none() {
        // Generic rows need the full removal path so row teardown
        // goes through the same cleanup surface that mounted them.
        return Err(old_prior);
    }

    let mut removed_idx: Option<usize> = None;
    let mut new_i = 0usize;
    for old_i in 0..old_len {
        if new_i < total && Object::is(&old_prior[old_i].item_value, &arr.get(new_i as u32)) {
            new_i += 1;
            continue;
        }
        if removed_idx.is_some() {
            return Err(old_prior);
        }
        removed_idx = Some(old_i);
    }
    if new_i != total {
        return Err(old_prior);
    }
    let Some(removed_idx) = removed_idx else {
        return Err(old_prior);
    };
    if old_prior[removed_idx].leaving
        || crate::directives::transition::has_transition_in_subtree(&old_prior[removed_idx].element)
    {
        return Err(old_prior);
    }

    let row_iter_start = crate::profiler::reconcile::start();
    let mut fresh = Vec::with_capacity(total);
    let mut removed: Option<PrevItem> = None;
    for (old_i, mut entry) in old_prior.into_iter().enumerate() {
        if old_i == removed_idx {
            removed = Some(entry);
            continue;
        }
        let index = fresh.len();
        update_reused_entry(
            &mut entry,
            index,
            total,
            compiled_bindings_depend_on_position,
        );
        fresh.push(entry);
    }
    crate::profiler::reconcile::record_row_iter(row_iter_start);

    let leaver_drain_start = crate::profiler::reconcile::start();
    if let Some(entry) = removed {
        if !entry.leaving {
            if let Some(parent) = entry.element.parent_node() {
                let _ = parent.remove_child(&entry.element);
            }
            crate::directives::for_plan::unmount_row_compiled(entry.scope_id);
        }
    }
    crate::profiler::reconcile::record_leaver_drain(leaver_drain_start);

    Ok(fresh)
}

#[allow(clippy::too_many_arguments)]
fn try_two_swap_fast_path(
    arr: &Array,
    total: usize,
    mut old_prior: Vec<PrevItem>,
    parent_node: &Node,
    row_plan: Option<&Rc<crate::directives::for_plan::CompiledRowPlan>>,
    compiled_bindings_depend_on_position: bool,
) -> Result<Vec<PrevItem>, Vec<PrevItem>> {
    if total < 2 || old_prior.len() != total {
        return Err(old_prior);
    }
    if row_plan.is_none() {
        // Generic rows still use the full keyed path because their
        // cleanup/reuse contract is not the compiled-row one.
        return Err(old_prior);
    }
    if old_prior.iter().any(|entry| entry.leaving) {
        return Err(old_prior);
    }
    let mut mismatches: Vec<usize> = Vec::with_capacity(2);
    for i in 0..total {
        if !Object::is(&old_prior[i].item_value, &arr.get(i as u32)) {
            mismatches.push(i);
            if mismatches.len() > 2 {
                return Err(old_prior);
            }
        }
    }
    if mismatches.len() != 2 {
        return Err(old_prior);
    }
    let a = mismatches[0];
    let b = mismatches[1];
    if !Object::is(&old_prior[a].item_value, &arr.get(b as u32))
        || !Object::is(&old_prior[b].item_value, &arr.get(a as u32))
    {
        return Err(old_prior);
    }

    let row_iter_start = crate::profiler::reconcile::start();
    old_prior.swap(a, b);
    update_reused_entry(
        &mut old_prior[a],
        a,
        total,
        compiled_bindings_depend_on_position,
    );
    update_reused_entry(
        &mut old_prior[b],
        b,
        total,
        compiled_bindings_depend_on_position,
    );
    crate::profiler::reconcile::record_row_iter(row_iter_start);

    let reorder_start = crate::profiler::reconcile::start();
    let dom_insert_start = crate::profiler::mount::start();
    let moved_to_front = old_prior[a].element.clone();
    let moved_to_back = old_prior[b].element.clone();
    let back_next = moved_to_front.next_sibling();
    let _ = parent_node.insert_before(moved_to_front.as_ref(), Some(moved_to_back.as_ref()));
    let _ = parent_node.insert_before(moved_to_back.as_ref(), back_next.as_ref());
    crate::profiler::mount::record_dom_insertion(dom_insert_start);
    crate::profiler::reconcile::record_reorder(reorder_start);

    Ok(old_prior)
}

/// Keyed iteration. Reuses clones whose keys still appear, fires
/// `trigger_scope` so their bindings re-evaluate against the updated
/// `LoopScope`, and reorders the DOM to match the new order.
#[allow(clippy::too_many_arguments)]
fn run_keyed(
    item_name: &'static str,
    items_expr: &'static str,
    key_expr: &'static str,
    parent_proxy: JsValue,
    parent_scope_id: ScopeId,
    inject_parent_id: ScopeId,
    template: ForTemplate,
    anchor: Node,
    stagger_ms: u32,
    body: Option<crate::directives::for_plan::ForBodyFn>,
    row_plan: Option<Rc<crate::directives::for_plan::CompiledRowPlan>>,
) -> crate::reactive::EffectId {
    let key_resolver = KeyResolver::parse(item_name, key_expr);
    // RFC 054 — opportunistic compiled-row-plan fast path. Looked
    // up once per `pp-for` directive bind (NOT per row). Templates
    // the macro flagged as eligible (flat keyed table rows, no
    // nested directives, supported binding/listener envelope) are
    // stamped with `data-pp-row-plan="<id>"`. None ⇒ generic
    // mount; Some ⇒ skip the per-row attribute scan and patch
    // dynamic nodes from the plan directly.

    // RFC 054 — proxy elision. Plans whose every binding routes
    // through the typed `FastExpr` evaluator never read the row's
    // `js_sys::Proxy` on the per-row hot path. Skipping
    // `Scope::into_proxy` (Object::new ×2 + 2 trap closures +
    // Proxy::new + 2 wasm-js bridge `Reflect::set` calls) per row
    // is ~24K bridge ops on `runLots(10000)`. The proxy is
    // lazy-minted by `enclosing_scope` / `instance_proxy` if a
    // delegated listener actually fires, so the rare interactive
    // case still works.
    let elide_proxy = row_plan
        .as_ref()
        .map(|p| p.is_proxy_elision_eligible())
        .unwrap_or(false);
    let compiled_bindings_depend_on_position = row_plan
        .as_ref()
        .map(|p| p.depends_on_loop_position())
        .unwrap_or(true);
    let item_name: Rc<str> = item_name.into();
    let prior: Rc<RefCell<Vec<PrevItem>>> = Rc::new(RefCell::new(Vec::new()));
    // Pool + seen-keys set carry allocated capacity across effect
    // re-runs. Both are fully drained at the end of each run so
    // reusing them keeps rehash / grow costs out of the hot path
    // for long-lived lists (N items reconciled K times allocates
    // once, not K times).
    let pool_cell: Rc<RefCell<HashMap<RowKey, PrevItem>>> = Rc::new(RefCell::new(HashMap::new()));
    let seen_cell: Rc<RefCell<HashSet<RowKey>>> = Rc::new(RefCell::new(HashSet::new()));
    // RFC-095 W1 — see run_naive's items_reader note.
    let items_reader = crate::scope::scoped_root_reader(parent_scope_id);

    effect(move || {
        let reconcile_total_start = crate::profiler::reconcile::start();
        let items_js =
            crate::path::resolve_path_with(&parent_proxy, items_reader.as_ref(), items_expr);
        let arr: Array = items_js
            .dyn_into::<Array>()
            .unwrap_or_else(|_| Array::new());
        let total = arr.length() as usize;

        let Some(parent_node) = anchor.parent_node() else {
            // Anchor not attached — clear any tracking.
            prior.borrow_mut().clear();
            crate::profiler::reconcile::record_total(reconcile_total_start);
            return;
        };
        let parent_node_ref: &Node = parent_node.as_ref();
        if let Some(plan) = row_plan.as_ref() {
            crate::directives::for_plan::ensure_delegated_listeners(
                plan,
                &template.anchor(),
                parent_node_ref,
            );
        }

        let old_prior: Vec<PrevItem> = {
            let mut b = prior.borrow_mut();
            std::mem::take(&mut *b)
        };
        let old_prior = match try_append_fast_path(
            &arr,
            total,
            old_prior,
            &seen_cell,
            &key_resolver,
            &item_name,
            &parent_proxy,
            parent_scope_id,
            inject_parent_id,
            &template,
            &anchor,
            parent_node_ref,
            body,
            elide_proxy,
            row_plan.as_ref(),
            compiled_bindings_depend_on_position,
            stagger_ms,
        ) {
            Ok(next_prior) => {
                *prior.borrow_mut() = next_prior;
                crate::profiler::reconcile::record_total(reconcile_total_start);
                return;
            }
            Err(old_prior) => old_prior,
        };
        let old_prior = match try_prepend_fast_path(
            &arr,
            total,
            old_prior,
            &seen_cell,
            &key_resolver,
            &item_name,
            &parent_proxy,
            parent_scope_id,
            inject_parent_id,
            &template,
            parent_node_ref,
            body,
            elide_proxy,
            row_plan.as_ref(),
            compiled_bindings_depend_on_position,
            stagger_ms,
        ) {
            Ok(next_prior) => {
                *prior.borrow_mut() = next_prior;
                crate::profiler::reconcile::record_total(reconcile_total_start);
                return;
            }
            Err(old_prior) => old_prior,
        };
        let old_prior = match try_single_remove_fast_path(
            &arr,
            total,
            old_prior,
            row_plan.as_ref(),
            compiled_bindings_depend_on_position,
        ) {
            Ok(next_prior) => {
                *prior.borrow_mut() = next_prior;
                crate::profiler::reconcile::record_total(reconcile_total_start);
                return;
            }
            Err(old_prior) => old_prior,
        };
        let old_prior = match try_two_swap_fast_path(
            &arr,
            total,
            old_prior,
            parent_node_ref,
            row_plan.as_ref(),
            compiled_bindings_depend_on_position,
        ) {
            Ok(next_prior) => {
                *prior.borrow_mut() = next_prior;
                crate::profiler::reconcile::record_total(reconcile_total_start);
                return;
            }
            Err(old_prior) => old_prior,
        };

        // Drain prior into a key → entry map so we can look up reuse
        // candidates in O(1).
        let pool_build_start = crate::profiler::reconcile::start();
        let mut pool = pool_cell.borrow_mut();
        pool.clear();
        let pool_cap = pool.capacity();
        if pool_cap < total {
            pool.reserve(total - pool_cap);
        }
        // Clearing a compiled list does not need keyed lookup at all:
        // every prior row is leaving. Try the same safe bulk teardown
        // before hashing 10K keys into `pool`; fall back to the normal
        // reconcile path if transitions or sibling structure require it.
        if total == 0 && row_plan.is_some() {
            if let Some(parent_el) = parent_node.dyn_ref::<Element>() {
                crate::profiler::reconcile::record_pool_build(pool_build_start);
                let leaver_drain_start = crate::profiler::reconcile::start();
                if bulk_clear_compiled(parent_el, &old_prior, &anchor) {
                    prior.borrow_mut().clear();
                    crate::profiler::reconcile::record_leaver_drain(leaver_drain_start);
                    crate::profiler::reconcile::record_total(reconcile_total_start);
                    return;
                }
            }
        }
        for entry in old_prior {
            pool.insert(entry.key.clone(), entry);
        }

        // Seen-keys set replaces the old O(N²) `fresh.iter().any()`
        // duplicate scan with O(1) lookups. For lists of 1000+
        // items this was the dominant cost of a reconcile.
        let mut seen = seen_cell.borrow_mut();
        seen.clear();
        let seen_cap = seen.capacity();
        if seen_cap < total {
            seen.reserve(total - seen_cap);
        }
        crate::profiler::reconcile::record_pool_build(pool_build_start);

        let mut fresh: Vec<PrevItem> = Vec::with_capacity(total);
        // RFC 054 — when there's no prior pool, every iteration
        // hits the "New" branch unconditionally. The
        // `item_signature` JSON.stringify, the `seen.insert`
        // dedup check, and the `pool.remove` lookup are all pure
        // overhead in that case (`item_sig` only matters across
        // reconciles via `entry.item_sig`, which doesn't exist
        // for fresh rows; `seen` only matters when reuse is
        // possible). For `runLots(10000)`'s initial mount this
        // saves ~30-50ms.
        let pool_initially_empty = pool.is_empty();

        // RFC 054 Lever 6 was a bulk-mount via parsed
        // `<template>.innerHTML` — net negative across engines
        // (Chromium pays a ~12× attach penalty for HTML-parser
        // elements, see `jsbench/RESULTS.md`). The
        // suffix-batch path below (`clone_template_body` × N
        // into a `DocumentFragment`, single `insert_before`)
        // handles the empty-pool initial mount instead.

        let row_iter_start = crate::profiler::reconcile::start();
        // RFC-095 W4 — cold mount through the mutation channel:
        // when every row is new, the per-row DOM work (clone,
        // scope stamp, binding writes, fragment append, insert)
        // collapses into one interpreter crossing. Key resolution
        // and dedup stay Rust-side, identical to the loop below.
        if pool_initially_empty && total > 0 && elide_proxy && crate::mutation_channel::enabled() {
            if let (Some(plan), Some(proto)) = (row_plan.as_ref(), template.prototype()) {
                if let Some(slot) = crate::directives::for_plan::channel_slot(plan) {
                    let mut pairs: Vec<(RowKey, JsValue)> = Vec::with_capacity(total);
                    for i in 0..total {
                        let item = arr.get(i as u32);
                        let key_val = key_resolver.resolve(&item, i, &parent_proxy);
                        let key = dedup_row_key(&key_val, &mut seen, i);
                        pairs.push((key, item));
                    }
                    crate::profiler::reconcile::record_row_iter(row_iter_start);
                    let reorder_start = crate::profiler::reconcile::start();
                    let fresh = channel_mount_range(
                        plan,
                        slot,
                        &proto,
                        &anchor,
                        &arr,
                        0,
                        total,
                        pairs,
                        &item_name,
                        parent_scope_id,
                        inject_parent_id,
                        stagger_ms,
                    );
                    crate::profiler::reconcile::record_reorder(reorder_start);
                    if let Some(fresh) = fresh {
                        drop(seen);
                        *prior.borrow_mut() = fresh;
                        crate::profiler::reconcile::record_total(reconcile_total_start);
                        return;
                    }
                    // Interpreter refused — clear dedup state and
                    // fall through to the direct mount loop below.
                    seen.clear();
                }
            }
        }
        for i in 0..total {
            let item = arr.get(i as u32);
            let key_val = key_resolver.resolve(&item, i, &parent_proxy);
            // Cold-pool optimisation: skip the `item_signature`
            // (only meaningful across reconciles) and the
            // `pool.remove` (always None when pool is empty).
            // Dedup still has to run — duplicate keys must be
            // disambiguated on first mount too, otherwise the
            // *next* reconcile drains `prior` into `pool` via
            // `HashMap::insert` and silently overwrites the
            // earlier entries, losing row + scope tracking.
            let item_sig: String = if pool_initially_empty {
                String::new()
            } else {
                // Reused rows compute their signature only after
                // `pool.remove` below. If the cached JS row object is
                // identical, the row data cannot have changed from the
                // DOM's point of view and JSON.stringify is pure waste.
                String::new()
            };
            let key = dedup_row_key(&key_val, &mut seen, i);

            let pool_lookup = if pool_initially_empty {
                None
            } else {
                pool.remove(&key)
            };
            if let Some(mut entry) = pool_lookup {
                // Reuse. If the entry was mid-leave (prior reconcile
                // started its unmount but the transition hadn't
                // finished), cancel the leave by running enter — the
                // clone is still in the DOM and its scope is intact.
                if entry.leaving {
                    restore_leaver_layout(&entry);
                    crate::directives::transition::enter_subtree(&entry.element, || {});
                    entry.leaving = false;
                }
                let same_item = Object::is(&entry.item_value, &item);
                let mut next_item_sig = entry.item_sig.clone();
                let item_changed = if same_item {
                    false
                } else {
                    next_item_sig = item_signature(&item);
                    next_item_sig != entry.item_sig
                };
                let position_changed = {
                    let st = entry.loop_state.borrow();
                    st.index != i || st.total != total
                };
                let needs_loop_update = position_changed || !same_item;
                let needs_binding_update = item_changed
                    || (position_changed
                        && (row_plan.is_none() || compiled_bindings_depend_on_position));
                if needs_loop_update {
                    let item_for_entry = item.clone();
                    {
                        let mut st = entry.loop_state.borrow_mut();
                        st.item = item;
                        st.index = i;
                        st.total = total;
                    }
                    entry.item_value = item_for_entry;
                    entry.item_sig = next_item_sig;
                }
                if needs_binding_update {
                    // RFC 054 §5.5 — compiled rows skip the
                    // reactive sweep; their bindings evaluate
                    // directly against the mutated loop state and
                    // patch DOM only on cache miss. Generic rows
                    // still go through `trigger_scope` so any
                    // effect-wrapped binding fires once.
                    if !crate::directives::for_plan::reuse_row_compiled(entry.scope_id) {
                        trigger_scope(entry.scope_id);
                    }
                }
                fresh.push(entry);
            } else {
                // New. Fresh loop scope + clone.
                let item_sig = if pool_initially_empty {
                    item_sig
                } else {
                    item_signature(&item)
                };
                let loop_rc = Rc::new(RefCell::new(LoopScope {
                    item_name: Rc::clone(&item_name),
                    item: item.clone(),
                    index: i,
                    total,
                    parent_scope_id,
                }));
                let scope = Scope::new(loop_rc.clone());
                crate::context::set_parent(scope.id, inject_parent_id);

                let clone_start = crate::profiler::mount::start();
                // RFC-058 Phase 4.2c — when no row plan claims
                // this site AND the macro emitted a body
                // fragment, build the row via the fragment
                // (stamps cleaned HTML + installs every
                // directive against the row's LoopScope via
                // the Phase 1 helpers — no `mount::walk`
                // involvement). Otherwise clone the template
                // body and let the install loop run
                // `mount::walk` / `mount_row_compiled` as
                // before.
                let clone_root = if row_plan.is_none() {
                    if let Some(body_fn) = body {
                        let proxy = scope.into_proxy();
                        match body_fn(scope.id, &proxy, scope.id) {
                            Some(root) => {
                                bind_scope_to(&root, scope.id, &proxy);
                                Some(root)
                            }
                            None => {
                                console::error_1(&JsValue::from_str(
                                    "pp-for: row body fragment failed to materialise root",
                                ));
                                Scope::remove(scope.id);
                                break;
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let clone_root = match clone_root {
                    Some(root) => root,
                    None => {
                        let Some(root) = template.clone_body() else {
                            console::error_1(&JsValue::from_str(
                                "pp-for: <template> body must contain exactly one element",
                            ));
                            Scope::remove(scope.id);
                            break;
                        };
                        if elide_proxy {
                            mount::bind_scope_id_only(&root, scope.id);
                        } else {
                            let proxy = scope.into_proxy();
                            bind_scope_to(&root, scope.id, &proxy);
                        }
                        // RFC-058 Phase 6.5 — stamp `CTX_PARENT_KEY`
                        // = LoopScope so any custom tag inside the
                        // cloned row body chains its inject parent
                        // through this row's scope. Mirrors what
                        // `stamp_if_body` does for the body_fn
                        // path.
                        let ctx_key = wasm_bindgen::JsValue::from_str(mount::CTX_PARENT_KEY);
                        let ctx_val = wasm_bindgen::JsValue::from_f64(scope.id.0 as f64);
                        let _ = js_sys::Reflect::set(root.as_ref(), &ctx_key, &ctx_val);
                        root
                    }
                };
                crate::profiler::mount::record_clone_template_body(clone_start);
                fresh.push(PrevItem {
                    element: clone_root,
                    scope_id: scope.id,
                    loop_state: loop_rc,
                    key,
                    item_value: item,
                    item_sig,
                    leaving: false,
                });
            }
        }
        crate::profiler::reconcile::record_row_iter(row_iter_start);

        // Anything left in the pool is no longer in the iteration
        // source. RFC-038: route through `transition::leave_subtree`
        // so any animated descendants (Pine compounds with
        // `transition = "..."`) play their leave keyframes before
        // the actual remove_child fires.
        //
        // Crucially, we KEEP leaving entries tracked in `prior` until
        // their remove callback fires. If an item's key reappears
        // while its clone is still mid-leave (rapid mount→unmount→
        // mount on a stress list), the next reconcile's pool includes
        // the leaving entry, matches the key, cancels the leave, and
        // reuses the clone — instead of spawning a duplicate next to
        // the still-leaving original. The remove callback retracts
        // the entry from `prior` once the clone is truly gone.
        let mut leaver_flip_snapshots: HashMap<RowKey, (Element, web_sys::DomRect)> =
            if pool.is_empty() {
                HashMap::new()
            } else {
                let mut snapshots = HashMap::new();
                for entry in &fresh {
                    let Some(target) = flip_target_for_entry(entry) else {
                        continue;
                    };
                    snapshots.insert(
                        entry.key.clone(),
                        (target.clone(), target.get_bounding_client_rect()),
                    );
                }
                snapshots
            };

        let has_leavers = !pool.is_empty();
        let n_new: usize = fresh.len();
        let mut leavers: Vec<PrevItem> = Vec::new();
        let leaver_drain_start = crate::profiler::reconcile::start();

        // RFC 054 — bulk-clear fast path. When the new list is
        // empty, every entry in the pool is a sync-leaver (no
        // transitions), and the only Element children of
        // `parent_node` are our clones, do ONE
        // `replaceChildren(anchor)` instead of
        // `parent.remove_child(&el)` × N. For 10K-row clear the
        // diagnostic profile pinned 387ms in this loop alone —
        // the bridge cost of N individual `remove_child` calls
        // dwarfed everything else.
        //
        // Gated on `row_plan.is_some()`: the bulk teardown
        // (`unmount_rows_bulk` + `Scope::remove_compiled_rows`
        // + `mark_bulk_release`) skips refs/slots/ids/context/
        // tasks/component_computed/model_runtime cleanup AND
        // suppresses the `MutationObserver`-driven
        // `release_subtree` sweep. Compiled rows are guaranteed
        // not to register any of those side-tables; generic rows
        // can. Routing generic rows here would leak every one of
        // those tables for the entire torn-down list.
        if total == 0 && !pool.is_empty() && row_plan.is_some() {
            if let Some(parent_el) = parent_node.dyn_ref::<Element>() {
                let pool_count = pool.len();
                // Strict guard: `parent_node` owns *exactly* the
                // pool's clones plus the anchor, and any other
                // non-element children are whitespace-only
                // (typical template indentation). Foreign
                // comments, user text, or unexpected element siblings
                // (e.g. a `<tr class="header">` next to a
                // `<template pp-for>` inside the same `<tbody>`)
                // bail out — `replace_children_with_node_1`
                // below would otherwise nuke them.
                if bulk_clear_safe(parent_el, pool.values(), &anchor, pool_count) {
                    let scope_ids: Vec<ScopeId> =
                        pool.values().map(|entry| entry.scope_id).collect();
                    // Cleanup BEFORE the DOM mutation:
                    //  1. Stamp every clone with the
                    //     `release_skip` marker — when the
                    //     `MutationObserver`-driven
                    //     `release_subtree` fires async on each
                    //     removed row, the marker short-circuits
                    //     the per-element side-table sweep.
                    //     11K rows × ~5 sub-elements × ~10
                    //     `Reflect::get` calls otherwise dominate
                    //     `clear`'s tail.
                    //  2. Remove each scope from the registry +
                    //     drop its `RowInstance`. Idempotent if
                    //     `release_subtree` ever did fire its
                    //     normal path (it won't, given (1)).
                    // RFC 054 Lever 5b — single parent-level
                    // marker instead of N per-row stamps. The
                    // `MutationObserver` callback honors
                    // `BULK_RELEASE_KEY` on `rec.target()` and
                    // skips the entire batch's `release_subtree`
                    // sweep, then clears the marker. One
                    // `Reflect::set` per clear instead of 10K.
                    // RFC-058 Phase 6.5 — MutationObserver-driven release-skip
                    // marker is gone with the mount. Synchronous bulk teardown
                    // does the work directly below.
                    // RFC 054 Lever 5 — bulk teardown. The per-row
                    // unmount path was O(N²) due to
                    // `LIST_WATCHERS.members.retain(|id| id != sid)`
                    // running once per row over an N-element Vec
                    // (~358ms slowest run for 10K rows). The bulk
                    // variant drops the entire watcher in one
                    // borrow and drains side-tables in one
                    // `thread_local::with` per table.
                    crate::directives::for_plan::unmount_rows_bulk(&scope_ids);
                    Scope::remove_compiled_rows(&scope_ids);
                    pool.clear();
                    parent_el.replace_children_with_node_1(&anchor);
                    prior.borrow_mut().clear();
                    crate::profiler::reconcile::record_leaver_drain(leaver_drain_start);
                    crate::profiler::reconcile::record_total(reconcile_total_start);
                    return;
                }
            }
        }

        for (_, mut entry) in pool.drain() {
            if !entry.leaving {
                entry.leaving = true;
                lift_leaver_out_of_layout(&entry);
                let el = entry.element.clone();
                let el_for_cb = el.clone();
                let scope_id_for_unmount = entry.scope_id;
                let key_for_retract = entry.key.clone();
                let prior_for_retract = prior.clone();
                if !crate::directives::transition::has_transition_in_subtree(&el) {
                    if let Some(parent) = el.parent_node() {
                        let _ = parent.remove_child(&el);
                    }
                    crate::directives::for_plan::unmount_row_compiled(scope_id_for_unmount);
                    retract_from_prior(&prior_for_retract, &key_for_retract);
                    // No transition → the entry is fully retired
                    // synchronously. Skipping the `leavers.push`
                    // below keeps the dead entry out of
                    // `fresh.extend(leavers)`, which would
                    // otherwise re-stash it into `prior` and let
                    // the *next* reconcile reuse the clone after
                    // its scope was already removed by the async
                    // observer — `mount_row_compiled` would then
                    // run with `loop_state = None` and every
                    // binding would evaluate to UNDEFINED.
                    continue;
                }
                crate::directives::transition::leave_subtree(&el, move || {
                    if let Some(parent) = el_for_cb.parent_node() {
                        let _ = parent.remove_child(&el_for_cb);
                    }
                    crate::directives::for_plan::unmount_row_compiled(scope_id_for_unmount);
                    retract_from_prior(&prior_for_retract, &key_for_retract);
                });
            }
            leavers.push(entry);
        }
        crate::profiler::reconcile::record_leaver_drain(leaver_drain_start);

        // Short-circuit: if the DOM already matches the new order
        // (every `fresh[i]` is parented at `parent_node` AND its
        // next sibling is `fresh[i+1]` or the anchor for the
        // last slot), there's nothing to do and we skip the entire
        // reorder pass. Otherwise fall through to an unconditional
        // insert loop — per-iter "skip if in place" is locally
        // correct but globally wrong during a reorder (can leave
        // elements stranded in their old positions).
        // Walk next-siblings past any still-present leaving
        // clones so the "correct next" check lines up with what
        // the layout will be once the leave animations finish.
        // Without this, deleting the last (or any tail-adjacent)
        // item in a keyed list flips `already_ordered` to false
        // — the insert loop then reshuffles every fresh clone
        // past the leaving element, and FLIP fires on items that
        // wouldn't actually have moved if we'd just let the
        // leaver finish and drop.
        fn next_non_leaving(node: Option<web_sys::Node>) -> Option<web_sys::Node> {
            let mut cursor = node;
            while let Some(n) = cursor.clone() {
                if let Ok(el) = n.dyn_into::<Element>() {
                    if crate::directives::transition::is_leaving(&el) {
                        cursor = el.next_sibling();
                        continue;
                    }
                }
                return cursor;
            }
            None
        }
        let already_ordered = fresh.iter().enumerate().all(|(i, entry)| {
            let correct_parent = entry
                .element
                .parent_node()
                .map(|p| p.is_same_node(Some(parent_node_ref)))
                .unwrap_or(false);
            let expected_next: &Node = if i + 1 < fresh.len() {
                fresh[i + 1].element.as_ref()
            } else {
                &anchor
            };
            let correct_next = next_non_leaving(entry.element.next_sibling())
                .map(|n| n.is_same_node(Some(expected_next)))
                .unwrap_or(false);
            correct_parent && correct_next
        });

        // RFC-038 FLIP prep — snapshot client rects for every
        // reused clone BEFORE the insert_before loop below moves
        // them. We only bother when there's actually a reorder
        // incoming; `already_ordered` skips to keep no-op sweeps
        // (an unrelated field changed, trigger_scope fired, but
        // the list hasn't moved) from paying N forced layouts.
        //
        // For Pine compounds the clone_root is an outer custom
        // element (`<pine-tags-input-item>`) with no display box —
        // `getBoundingClientRect` on it returns zero. The visible
        // layout box lives on the inner rendered root (the first
        // element child). Snapshot + animate that.
        let mut flip_snapshots: HashMap<RowKey, (Element, web_sys::DomRect)> = HashMap::new();
        if has_leavers {
            flip_snapshots = std::mem::take(&mut leaver_flip_snapshots);
        } else if !already_ordered {
            for entry in &fresh {
                let Some(target) = flip_target_for_entry(entry) else {
                    continue;
                };
                flip_snapshots.insert(
                    entry.key.clone(),
                    (target.clone(), target.get_bounding_client_rect()),
                );
            }
        }

        let mut newly_walked: Vec<Element> = Vec::new();
        let reorder_start = crate::profiler::reconcile::start();
        if !already_ordered {
            let suffix_insert_start = fresh
                .iter()
                .position(|entry| entry.element.parent_node().is_none())
                .unwrap_or(fresh.len());
            let can_batch_suffix = !has_leavers
                && suffix_insert_start < fresh.len()
                && fresh[suffix_insert_start..]
                    .iter()
                    .all(|entry| entry.element.parent_node().is_none())
                && fresh[..suffix_insert_start]
                    .iter()
                    .enumerate()
                    .all(|(i, entry)| {
                        let correct_parent = entry
                            .element
                            .parent_node()
                            .map(|p| p.is_same_node(Some(parent_node_ref)))
                            .unwrap_or(false);
                        let expected_next: &Node = if i + 1 < fresh.len() {
                            fresh[i + 1].element.as_ref()
                        } else {
                            &anchor
                        };
                        let correct_next = next_non_leaving(entry.element.next_sibling())
                            .map(|n| n.is_same_node(Some(expected_next)))
                            .unwrap_or(false);
                        correct_parent && correct_next
                    });
            if can_batch_suffix {
                let doc = anchor
                    .owner_document()
                    .expect("anchor should belong to a document");
                let fragment: DocumentFragment = doc.create_document_fragment();
                let dom_insert_start = crate::profiler::mount::start();
                for entry in &fresh[suffix_insert_start..] {
                    let _ = fragment.append_child(entry.element.as_ref());
                    newly_walked.push(entry.element.clone());
                }
                let _ = parent_node.insert_before(fragment.as_ref(), Some(&anchor));
                crate::profiler::mount::record_dom_insertion(dom_insert_start);
            } else {
                // Iterate back-to-front and use the next fresh entry
                // (or the comment anchor for the last) as the insert
                // position.
                // That keeps leaving siblings in their original DOM
                // slots instead of pushing every fresh item past them
                // — deleting a middle item no longer reorders the
                // flex flow around the leaver, so FLIP only plays on
                // items whose slot actually moves.
                //
                // Per-iter skip: back-to-front means by the time we
                // consider fresh[i], fresh[i+1] is already in its
                // final position. So we only need to move fresh[i]
                // if its next sibling isn't already fresh[i+1]
                // (skipping any leavers in between). A rotate of N
                // items where only one moved used to do N no-op
                // insert_before round-trips through `remove + insert`
                // — each one still blurs / restarts transitions /
                // invalidates layout. Now it does exactly the moves
                // the mutation demands.
                // Back-to-front insert order keeps the local skip
                // check correct during a reorder. But the stagger
                // enter downstream wants clones in FORWARD order so a
                // 500-item mount paints top → bottom, not bottom → top.
                // Record new clones into a separate Vec indexed by
                // fresh position so we can restore the forward order
                // after this loop.
                let mut new_indices: Vec<usize> = Vec::new();
                let dom_insert_start = crate::profiler::mount::start();
                for i in (0..fresh.len()).rev() {
                    let entry = &fresh[i];
                    let was_in_place = entry
                        .element
                        .parent_node()
                        .map(|p| p.is_same_node(Some(parent_node_ref)))
                        .unwrap_or(false);
                    let insert_anchor: &Node = if i + 1 < fresh.len() {
                        fresh[i + 1].element.as_ref()
                    } else {
                        &anchor
                    };
                    let already_here = was_in_place
                        && next_non_leaving(entry.element.next_sibling())
                            .map(|n| n.is_same_node(Some(insert_anchor)))
                            .unwrap_or(false);
                    if !already_here {
                        let _ =
                            parent_node.insert_before(entry.element.as_ref(), Some(insert_anchor));
                    }
                    if !was_in_place {
                        new_indices.push(i);
                    }
                }
                crate::profiler::mount::record_dom_insertion(dom_insert_start);
                new_indices.sort_unstable();
                newly_walked.extend(new_indices.into_iter().map(|i| fresh[i].element.clone()));
            }
        }
        // Walk freshly-inserted clones AFTER they're in the tree so
        // directive setup can look up the enclosing scope via parent
        // chain if it needs to.
        //
        // RFC 054 — the compiled fast path swaps the generic mount
        // for a plan-driven mount: clone is already in the DOM with
        // its scope bound, so we resolve the dynamic node paths
        // and install bindings/listeners directly. Eligibility is
        // checked at macro time and the template is stamped with
        // `data-pp-row-plan="<id>"`; `lookup_for_template` returns
        // `None` for any template that didn't qualify, and we fall
        // back to the generic mount.
        if let Some(plan) = &row_plan {
            let mut compiled_rows: Vec<(Element, ScopeId, Option<JsValue>)> =
                Vec::with_capacity(newly_walked.len());
            let mut generic_rows: Vec<Element> = Vec::new();
            for el in &newly_walked {
                // For compiled rows we already know the scope id
                // (stamped at clone time) and whether to pass a
                // proxy: elision means none was minted, so we pass
                // `None` and the row listener path lazy-mints on
                // first listener fire. Going through
                // `enclosing_scope` here would defeat elision by
                // triggering the lazy-mint path immediately.
                if let Some(sid) = mount::scope_id_of_element(el) {
                    let proxy_for_mount = if elide_proxy {
                        None
                    } else {
                        crate::mount::scope_of_element(el).map(|(_, p)| p)
                    };
                    compiled_rows.push((el.clone(), sid, proxy_for_mount));
                } else {
                    generic_rows.push(el.clone());
                }
            }
            crate::directives::for_plan::mount_rows_compiled(plan, &compiled_rows);
            for el in &generic_rows {
                crate::profiler::mount::record_generic_row_mounted();
                mount::finalize_compiled_subtree(el);
            }
        } else {
            for el in &newly_walked {
                crate::profiler::mount::record_generic_row_mounted();
                // RFC-058 Phase 6.5 — generic (non-row-plan)
                // rows either came from `body_fn` (already bound by
                // generated installs inside the body fragment fn)
                // or from `clone_template_body` (unbound). For
                // both, fire lifecycle on the row + scan for
                // registered custom tags inside (a no-op for
                // body_fn rows since child mounts already ran
                // during the body fragment's generated install,
                // but cheap).
                mount::finalize_compiled_subtree(el);
            }
        }
        // RFC-038 — fire enter on each newly-walked clone subtree
        // so a freshly-added TagsInput chip / DropdownMenu Item /
        // etc. plays its mount preset. When the author set
        // `pp-stagger="<ms>"`, space the enters by that per-item
        // delay instead of all firing at once.
        fire_staggered_enter(&newly_walked, stagger_ms);

        // RFC-038 — FLIP play phase. Runs AFTER the mount stamps
        // `data-pp-animate` + inserts, so the elements' new
        // positions are real.
        //
        // `flip_batch` handles the two-pass invert-reflow-play
        // dance in a single forced layout for the whole list —
        // the per-element work is ~4 inline style writes instead
        // of an `Element.animate()` call per item (WAAPI allocates
        // a fresh `Animation` object each time, which was the
        // dominant cost of a 500-item shuffle).
        if !flip_snapshots.is_empty() {
            let mut pending: Vec<crate::animate::FlipTarget> =
                Vec::with_capacity(flip_snapshots.len());
            for entry in &fresh {
                if let Some((target, old_rect)) = flip_snapshots.remove(&entry.key) {
                    let new_rect = target.get_bounding_client_rect();
                    pending.push(crate::animate::FlipTarget {
                        element: target,
                        old_rect,
                        new_rect,
                    });
                }
            }
            crate::animate::flip_batch(pending, crate::animate::FlipOptions::default());
        }

        crate::profiler::reconcile::record_reorder(reorder_start);

        // `prior` holds everything the next reconcile can reuse:
        // the `fresh` list (current iteration order) plus any clones
        // that are mid-leave. The leaver's remove callback will
        // retract it from `prior` once the leave finishes; until
        // then, a re-appearing key can cancel the leave and reuse
        // that clone instead of spawning a duplicate.
        let _ = n_new; // kept for clarity when reading the function
        fresh.extend(leavers);
        *prior.borrow_mut() = fresh;
        crate::profiler::reconcile::record_total(reconcile_total_start);
    })
}

/// Pre-compiled form of `pp-key="..."`. Parsed once at `run_keyed`
/// entry; dispatched per-item without re-parsing the expression.
/// The big win vs. the old `resolve_key(&str)` was dropping the
/// per-iteration `format!("{item_name}.")` + string-compare chain
/// — for a 500-item list that allocation + match fires 500 times
/// per reconcile, and is entirely redundant when the expression
/// hasn't changed.
enum KeyResolver {
    Index,
    Item,
    /// `item.id` — common keyed-table shape; avoid the generic
    /// segment vector and fold in the per-row hot path.
    ItemField(JsValue),
    /// `item.a.b.c` — pre-split JS property keys so we walk
    /// `Reflect::get` per segment without re-splitting or
    /// re-allocating a `JsValue` per item.
    ItemPath(Vec<JsValue>),
    /// Any other expression — falls through to `resolve_path`
    /// against the parent proxy (e.g. `$store.selected_id`).
    External(String),
}

impl KeyResolver {
    fn parse(item_name: &str, expr: &str) -> Self {
        let trimmed = expr.trim();
        if trimmed == "$index" {
            return Self::Index;
        }
        if trimmed == item_name {
            return Self::Item;
        }
        let prefix_len = item_name.len() + 1;
        if trimmed.len() > prefix_len
            && trimmed.starts_with(item_name)
            && trimmed.as_bytes().get(item_name.len()) == Some(&b'.')
        {
            let rest = &trimmed[prefix_len..];
            if !rest.contains('.') && !rest.is_empty() {
                return Self::ItemField(JsValue::from_str(rest));
            }
            return Self::ItemPath(
                rest.split('.')
                    .filter(|s| !s.is_empty())
                    .map(JsValue::from_str)
                    .collect(),
            );
        }
        Self::External(trimmed.to_string())
    }

    fn resolve(&self, item: &JsValue, index: usize, parent_proxy: &JsValue) -> JsValue {
        match self {
            Self::Index => JsValue::from_f64(index as f64),
            Self::Item => item.clone(),
            Self::ItemField(field) => Reflect::get(item, field).unwrap_or(JsValue::UNDEFINED),
            Self::ItemPath(segments) => segments.iter().fold(item.clone(), |acc, segment| {
                Reflect::get(&acc, segment).unwrap_or(JsValue::UNDEFINED)
            }),
            Self::External(path) => resolve_path(parent_proxy, path),
        }
    }
}

/// Canonicalise a key value to a string. Strings come through
/// unwrapped so adjacent hashes (`123` as number vs. string) don't
/// collide with their JSON-quoted form.
/// RFC-101 P3 — a row's reconcile key. The common case (`pp-key`
/// over numeric ids, e.g. jsbench `Row.id: usize`) is an integer that
/// carries NO heap allocation; everything else falls back to the
/// canonical string form (byte-identical to [`stringify_key`], so
/// string keys are unchanged on the wire).
///
/// Derived `Eq`/`Hash` discriminate on the variant first, so `Int(5)`
/// and `Str("5")` never compare equal or collide — which is also the
/// correct semantics (a stable `pp-key` expression keys a given list
/// on one shape, never both) and removes a latent collision the
/// all-`String` form had (numeric `5` stringified to `"5"` and *would*
/// have aliased a string key `"5"`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum RowKey {
    Int(i64),
    Str(Rc<str>),
}

impl std::fmt::Display for RowKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RowKey::Int(n) => write!(f, "{n}"),
            RowKey::Str(s) => f.write_str(s),
        }
    }
}

/// Largest f64 that round-trips exactly through an integer (2^53).
/// JS numbers beyond this lose integer precision, so two distinct ids
/// could alias to the same `i64` — those fall back to the string form.
const MAX_EXACT_INT_F64: f64 = 9_007_199_254_740_992.0;

/// Resolve a JS key value to a [`RowKey`]. Integral, in-range numbers
/// take the zero-alloc `Int` path; strings/bools/null/objects reuse
/// [`stringify_key`] verbatim for a byte-identical `Str` key.
fn row_key(v: &JsValue) -> RowKey {
    if let Some(n) = v.as_f64() {
        if n.is_finite() && n.fract() == 0.0 && n.abs() < MAX_EXACT_INT_F64 {
            return RowKey::Int(n as i64);
        }
    }
    RowKey::Str(stringify_key(v).into())
}

/// Resolve a row's key and disambiguate duplicates against `seen`,
/// matching the prior inline behaviour: a repeat key warns and is
/// re-mounted under a synthetic `…__dup_<i>` string key. Shared by
/// every append/prepend/mount construction site.
fn dedup_row_key(key_val: &JsValue, seen: &mut HashSet<RowKey>, i: usize) -> RowKey {
    let raw_key = row_key(key_val);
    if seen.insert(raw_key.clone()) {
        raw_key
    } else {
        console::warn_1(&JsValue::from_str(&format!(
            "pp-for: duplicate pp-key {raw_key} at index {i}; treating as new"
        )));
        let dup = RowKey::Str(format!("{raw_key}__dup_{i}").into());
        seen.insert(dup.clone());
        dup
    }
}

fn stringify_key(v: &JsValue) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    if let Some(s) = v.as_string() {
        return s;
    }
    if let Some(n) = v.as_f64() {
        return n.to_string();
    }
    if let Some(b) = v.as_bool() {
        return b.to_string();
    }
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .unwrap_or_default()
}

/// Clone `<template>.content` deeply and return the first element
/// child of the resulting fragment. Returns `None` when the body is
/// empty or has only non-element nodes.
fn clone_template_body(template: &HtmlTemplateElement) -> Option<Element> {
    template
        .content()
        .first_element_child()?
        .clone_node_with_deep(true)
        .ok()?
        .dyn_into::<Element>()
        .ok()
}

/// Parse `"item in items"`. Returns `None` on anything we don't want
/// to accept in v0 (destructuring, `(i, x) in ...`, empty halves).
///
/// Walker removal kept this helper for tests only — the macro now
/// pre-parses the expression at compile time and feeds the resolved
/// `(item_name, items_expr)` straight into `install`.
#[cfg(test)]
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
