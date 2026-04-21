//! DOM walker and `MutationObserver`.
//!
//! Pre-order walk of a subtree. For each element:
//!
//! 1. If its tag name matches a registered component, mount the
//!    component (clone template, apply static props, relocate slot content).
//! 2. Otherwise, run the existing pp-data / pp-* directive pass so
//!    server-rendered templates keep working too.
//!
//! Effects created by directives are pinned to their owning element so they
//! can be released on unmount via the `MutationObserver`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{
    DocumentFragment, Element, Event, EventTarget, HtmlTemplateElement, MutationObserver,
    MutationObserverInit, MutationRecord, Node, NodeList,
};

use crate::directives::{lookup, parse_attr, DirectiveCall};
use crate::reactive::{release, EffectId, ScopeId};
use crate::registry::instantiate;
use crate::scope::Scope;
use crate::slot_scope::SlotScope;
use crate::slots::{self, SlotStore, UserSlot};
use crate::templates::{is_registered, template_for};

const SCOPE_ID_KEY: &str = "__pp_scope_id";
const SCOPE_PROXY_KEY: &str = "__pp_scope_proxy";
const SCOPE_BORROWED_KEY: &str = "__pp_scope_borrowed";
const EFFECTS_KEY: &str = "__pp_effects";
const LISTENERS_KEY: &str = "__pp_listeners";
const INIT_PENDING_KEY: &str = "__pp_init_pending";
const WALKED_KEY: &str = "__pp_walked";
/// Explicit inject-chain parent for RFC-027. Stamped on
/// slot-materialised elements so their scopes chain to the slot-
/// *owning* component (the one whose template contains the
/// `<slot>`), not the *caller* that authored the slot content.
/// Needed for compound components — e.g. Radix-style DropdownMenu
/// where `<Trigger>` authored inside `<Root>` must inject from
/// `<Root>`, regardless of where the user's enclosing template
/// scope points.
const CTX_PARENT_KEY: &str = "__pp_ctx_parent";

/// Convenience used by `#[wasm_bindgen(js_name=start)]`.
pub fn start_on_body() {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Some(body) = doc.body() else { return };
    start(&body);
}

/// Walk `root`, bind directives on it and all descendants, then install a
/// `MutationObserver` so later DOM mutations are picked up too.
pub fn start(root: &Element) {
    crate::styles::inject_style(
        "__pp_cloak",
        "[pp-cloak] { display: none !important; }",
    );
    walk(root);
    install_observer(root);
}

/// Pin a pre-built scope onto an element so [`enclosing_scope`] resolves
/// through it. The element is assumed to **own** this scope — when the
/// element unmounts, `release_subtree` removes the scope from the
/// registry. Used by `pp-for`, which mints a fresh `LoopScope` per item.
pub fn bind_scope_to(el: &Element, scope_id: ScopeId, proxy: &JsValue) {
    set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope_id.0 as f64));
    set_private(el, SCOPE_PROXY_KEY, proxy);
}

/// Pin a **borrowed** scope. Same lookup semantics as `bind_scope_to`,
/// but `release_subtree` will leave the scope alone when this element
/// unmounts — the real owner is elsewhere. Used by `pp-teleport` and
/// the teleport path of `pp-if` to keep the enclosing component's
/// scope reachable from a clone that lives outside the component's
/// subtree.
pub fn bind_borrowed_scope_to(el: &Element, scope_id: ScopeId, proxy: &JsValue) {
    set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope_id.0 as f64));
    set_private(el, SCOPE_PROXY_KEY, proxy);
    set_private(el, SCOPE_BORROWED_KEY, &JsValue::TRUE);
}

/// Walk a subtree. Non-`pp-init` directives bind pre-order; `pp-init`
/// fires **post-order** — after every descendant has been walked — so
/// init handlers see a fully-bound subtree (children's refs, effects,
/// etc.). Public so the router can walk the custom-element tag it
/// creates inside an `<pp-outlet>`.
///
/// Marks the element with [`WALKED_KEY`] on completion so the
/// `MutationObserver` knows not to re-walk it when we reparent the
/// same node later (e.g. keyed `pp-for` reorders, `pp-teleport`).
///
/// Intercepts `<slot>` elements — per RFC-011, a slot is replaced
/// at walk time with either the user's captured content (via the
/// slot store keyed by the owning component) or the slot's own
/// default children, then the replacement is walked recursively.
pub fn walk(el: &Element) {
    if el.local_name() == "slot" {
        materialize_slot(el);
        return;
    }
    bind(el);
    // Snapshot children first — directives inside `bind` can mutate
    // the live `HTMLCollection` (e.g. slot materialisation replaces
    // a child mid-iteration).
    let children = el.children();
    let mut snapshot: Vec<Element> = Vec::with_capacity(children.length() as usize);
    for i in 0..children.length() {
        if let Some(c) = children.item(i) {
            snapshot.push(c);
        }
    }
    for child in snapshot {
        walk(&child);
    }
    fire_deferred_init(el);
    fire_mount_hook(el);
    set_private(el, WALKED_KEY, &JsValue::TRUE);
}

fn fire_deferred_init(el: &Element) {
    let Some(value) = get_private(el, INIT_PENDING_KEY).and_then(|v| v.as_string()) else {
        return;
    };
    let _ = Reflect::delete_property(el.as_ref(), &INIT_PENDING_KEY.into());
    let Some((scope_id, proxy)) = enclosing_scope(el) else { return };
    dispatch(el, &proxy, scope_id, "pp-init", &value);
}

/// Fire the component-level `on_mount` lifecycle hook on elements
/// that own a (non-borrowed) scope. Runs post-order so the handler
/// sees the fully-bound subtree (refs included).
///
/// `trigger_scope` fires afterwards **only when the component
/// actually defined `on_mount`** — otherwise the hook is a no-op
/// and the sweep would cascade through the subtree for nothing. For
/// recursive component trees (e.g. `<hn-comment>` in a comment
/// thread), a blanket sweep per mount amplifies to O(depth × nodes)
/// effect re-runs during initial render.
fn fire_mount_hook(el: &Element) {
    let Some(id_num) = get_private(el, SCOPE_ID_KEY).and_then(|v| v.as_f64()) else {
        return;
    };
    let borrowed = get_private(el, SCOPE_BORROWED_KEY)
        .map(|v| v.is_truthy())
        .unwrap_or(false);
    if borrowed {
        return;
    }
    let id = ScopeId(id_num as u64);
    let Some(scope) = Scope::find(id) else { return };

    // Snapshot both hook flags before borrowing — `has_on_mount`
    // reads through an immutable borrow, `mount()` needs a mutable
    // one, and we want to check `has_on_ready` without racing.
    let (has_mount, has_ready) = {
        let s = scope.state.borrow();
        (s.has_on_mount(), s.has_on_ready())
    };

    if has_mount {
        let ctx = crate::lifecycle::LifecycleContext::__new(el, id);
        crate::scope::with_current_scope_id(id, || {
            scope.state.borrow_mut().mount(ctx);
        });
        crate::reactive::trigger_scope(id);
    }

    if has_ready {
        // RFC-026/029: defer `on_ready` to the next microtask so
        // the surrounding walker frame has unwound and pp-if /
        // pp-teleport children have had a chance to commit. Own the
        // element by cloning into the closure so the fresh
        // `LifecycleContext` at invoke time borrows a live handle.
        // The hook is invoked through an IMMUTABLE borrow — proxy
        // reads inside the hook (watch_field, refs::get_on that
        // touches the proxy, `$event`) require state.borrow() on
        // the get trap, which is compatible with other immutable
        // borrows.
        let el_owned = el.clone();
        crate::tick::next(move || {
            let Some(scope) = Scope::find(id) else { return };
            let ctx = crate::lifecycle::LifecycleContext::__new(&el_owned, id);
            crate::scope::with_current_scope_id(id, || {
                scope.state.borrow().on_ready(ctx);
            });
        });
    }
}

fn bind(el: &Element) {
    // Step 0: `<pp-outlet>` is the router's mount point. Hand the
    // element over; don't try to bind directives or mount anything.
    let tag = el.local_name();
    if tag == "pp-outlet" {
        crate::router::set_outlet(el.clone());
        return;
    }

    // Step 1: tag-based mounting. If this element's tag name is a
    // registered component, clone its template in and wire up the scope
    // onto the template's root. Directives on the tag itself evaluate in
    // the parent's scope (handled by the standard pp-* pass below).
    //
    // The guard keys only on `__pp_mounted` — NOT `SCOPE_ID_KEY` — because
    // pp-for pins a `LoopScope` onto the clone root before walking, and
    // that clone root is often a registered component tag. Keying on
    // SCOPE_ID_KEY would mistake the loop scope for "already mounted"
    // and skip the component mount entirely.
    if is_registered(&tag) && get_private(el, "__pp_mounted").is_none() {
        mount_component(el, &tag);
    }

    // Snapshot all pp-* attributes — some directives mutate the element
    // (e.g. `set_attribute`) which would invalidate a live NamedNodeMap.
    //
    // Normalise RFC-020 shorthand (`:attr` → `pp-bind:attr`, `@event`
    // → `pp-on:event`) before the `pp-*` filter so authors can pick
    // either spelling — same directive dispatch either way.
    let mut pp_attrs: Vec<(String, String)> = Vec::new();
    let attrs = el.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = normalise_shorthand_attr(&a.name());
        if name.starts_with("pp-") {
            pp_attrs.push((name, a.value()));
        }
    }

    // Step 2: pp-data — the server-rendered path (the custom-element path
    // already bound its scope on the cloned template root, so pp-data there
    // will be absent / stripped).
    let has_data = pp_attrs.iter().any(|(n, _)| n == "pp-data");
    if has_data && get_private(el, SCOPE_ID_KEY).is_none() {
        if let Some((_, value)) = pp_attrs.iter().find(|(n, _)| n == "pp-data") {
            if let Some(scope) = instantiate(value) {
                let proxy = scope.into_proxy();
                set_private(el, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
                set_private(el, SCOPE_PROXY_KEY, &proxy);
            }
        }
    }

    // Resolve the enclosing scope for every other directive.
    let (scope_id, proxy) = match enclosing_scope(el) {
        Some(p) => p,
        None => return,
    };

    // Second pass: everything except pp-data and pp-init. pp-init is
    // stashed on the element and fired post-order in `walk` — by then
    // descendants have been bound so init handlers see refs, child
    // scopes, etc.
    let mut init_value: Option<String> = None;
    for (name, value) in &pp_attrs {
        if name == "pp-data" {
            continue;
        }
        if name == "pp-init" {
            init_value = Some(value.clone());
            continue;
        }
        dispatch(el, &proxy, scope_id, name, value);
    }
    if let Some(v) = init_value {
        set_private(el, INIT_PENDING_KEY, &JsValue::from_str(&v));
    }
    // Silence the unused-`proxy` warning — the deferred init path looks
    // it up again via `enclosing_scope` at fire time.
    let _ = scope_id;

    // RFC-025: scan direct text-node children for `{expr}` pairs and
    // install per-segment effects. Must run after directives bind so
    // `pp-text`-owned elements are already flagged and skipped.
    crate::directives::interp::scan_children(el, &proxy);

    // `pp-cloak` only exists to hide the element until binding completes.
    // Drop it now that directives have run so the global cloak CSS rule
    // stops matching.
    let _ = el.remove_attribute("pp-cloak");
}

/// Mount a registered component on `el`:
///  * capture the tag's current children as slot content,
///  * instantiate a fresh scope,
///  * apply static attribute props to the scope,
///  * clone the registered template into `el`,
///  * bind the scope to the template's root and strip its `pp-data`,
///  * forward fallthrough attrs onto the template root (RFC-010),
///  * move captured children into the first `<slot>` within the template.
fn mount_component(el: &Element, tag: &str) {
    // RFC-019 — `pp-as` hoists the user's single child element as
    // the rendered root, discarding the template's wrapper. Only
    // engages when all the structural constraints hold; otherwise
    // falls through to the normal mount path.
    if el.has_attribute("pp-as") && try_mount_component_as(el, tag) {
        return;
    }

    let Some(scope) = instantiate(tag) else { return };
    // Record the parent scope for RFC-027 `inject` chain-walks.
    // Prefer the explicit `CTX_PARENT_KEY` stamp — set by slot
    // materialisation on slot-inserted elements so compound-
    // component children chain to the slot *owner* (the component
    // whose template contained the `<slot>`), not the caller that
    // authored the content. Falls back to the DOM ancestry via
    // `enclosing_inject_parent`, which in turn prefers an ancestor's
    // `CTX_PARENT_KEY` over its `SCOPE_ID_KEY` — required for tags
    // nested *inside* a slot wrapper (e.g. `<pine-dialog-close>`
    // inside a `<div class="row">` inside Content's slot).
    let ctx_parent = get_private(el, CTX_PARENT_KEY)
        .and_then(|v| v.as_f64())
        .map(|n| ScopeId(n as u64))
        .or_else(|| enclosing_inject_parent(el));
    if let Some(parent_id) = ctx_parent {
        crate::context::set_parent(scope.id, parent_id);
    }
    // Apply static props BEFORE building the proxy so trigger doesn't fire
    // before any effect subscribes.
    apply_static_props(el, &scope);
    // RFC-030: fire `on_setup` — the component's pre-children-walk
    // hook where fields can be initialised from injected context.
    // Runs with CURRENT_SCOPE_ID bound so `inject` / `this` resolve.
    // The parent chain is already wired up above, and static props
    // are applied; template hasn't been cloned yet, so anything the
    // hook writes into the scope is visible on the first directive
    // bind.
    if scope.state.borrow().has_setup() {
        crate::scope::with_current_scope_id(scope.id, || {
            scope.state.borrow_mut().setup();
        });
    }
    let proxy = scope.into_proxy();

    // Capture slot content. Named slot templates go into the slot
    // store keyed by the component's scope id; everything else lands
    // in the default slot.
    let slot_store = capture_slots(el);
    slots::put(scope.id, slot_store);

    // Clone the registered template in. `set_inner_html` drops the
    // tag's former children, which is the "capture" side of the old
    // flow.
    let Some(html) = template_for(tag) else { return };
    el.set_inner_html(&html);

    // Bind scope to the template's root element and strip pp-data so the
    // recursive walker doesn't try to re-instantiate.
    if let Some(root) = first_element_child(el) {
        set_private(&root, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
        set_private(&root, SCOPE_PROXY_KEY, &proxy);
        let _ = root.remove_attribute("pp-data");

        // Fallthrough (RFC-010).
        apply_fallthrough_attrs(el, &root, &scope);

        // RFC-038 — if the component declared default transition
        // presets via `#[component(transition = "…")]` (or the
        // asymmetric `transition_in` / `transition_out` split), stamp
        // them on the OUTER custom-element tag (`el`). `pp-if` /
        // `pp-show` call `transition::enter`/`leave` on the clone
        // root — which IS the custom tag — so that's where the
        // directive's state machine looks for the six
        // `pp-transition:*` class attrs. Preset classes set opacity
        // / transform which propagate to the inner rendered root
        // via inheritance and shared stacking context.
        //
        // Author-side attrs on the custom tag win — apply_preset
        // only fills in what isn't already there. `pp-transition`
        // shorthand on the tag has already expanded in
        // `get_or_init` via `expand_preset_shorthand` by the time
        // this runs, so the "has six attrs?" guard covers both
        // hand-wired six-attr form AND author-shorthand form.
        let (tr_in, tr_out, ak) = {
            let s = scope.state.borrow();
            (
                s.transition_in_preset(),
                s.transition_out_preset(),
                s.animate_kind(),
            )
        };
        if !tr_in.is_empty() || !tr_out.is_empty() {
            let effective_in = if tr_in.is_empty() { "none" } else { tr_in };
            let effective_out = if tr_out.is_empty() { "none" } else { tr_out };
            let already_set = el.has_attribute("pp-transition:enter")
                || el.has_attribute("pp-transition:enter-start")
                || el.has_attribute("pp-transition:enter-end")
                || el.has_attribute("pp-transition:leave")
                || el.has_attribute("pp-transition:leave-start")
                || el.has_attribute("pp-transition:leave-end")
                || el.has_attribute("pp-transition")
                || el.has_attribute("pp-transition:in")
                || el.has_attribute("pp-transition:out");
            if !already_set {
                crate::animate::apply_preset(el, effective_in, effective_out);
            }
        }
        // Stamp `data-pp-animate="<kind>"` on the outer custom tag
        // so pp-for's keyed reconcile can cheaply check whether to
        // FLIP each reused clone without walking the scope tree.
        if !ak.is_empty() {
            let _ = el.set_attribute("data-pp-animate", ak);
        }
    }

    // Mark the tag as mounted so the recursive walk doesn't re-enter this
    // branch if, for some reason, the walker visits it again.
    set_private(el, "__pp_mounted", &JsValue::TRUE);
}

/// Attempt to mount `tag` on `el` in `pp-as` mode: hoist the tag's
/// single child element as the rendered root, merging the template
/// root's attributes onto it.
///
/// Returns `true` on success. Returns `false` when structural
/// constraints fail (not exactly one user element child, or the
/// template root isn't a simple `<tag><slot></slot></tag>` wrapper)
/// — caller falls back to the normal mount path.
fn try_mount_component_as(el: &Element, tag: &str) -> bool {
    // 1. Find the user's single element child. Text / comment nodes
    //    around it are ignored. Named-slot <template> elements are
    //    dropped.
    let user_root = match find_single_child_element_skipping_slot_templates(el) {
        Some(e) => e,
        None => {
            web_sys::console::warn_1(&JsValue::from_str(
                "pocopine: pp-as requires exactly one child element; ignoring",
            ));
            return false;
        }
    };

    // 2. Instantiate scope + apply static props from the tag's own
    //    attributes (same as the normal path). Parent lookup for
    //    RFC-027 inject prefers `CTX_PARENT_KEY` (slot owner) over
    //    `SCOPE_ID_KEY` (slot author) — see `enclosing_inject_parent`
    //    for the rationale.
    let Some(scope) = instantiate(tag) else { return false };
    let ctx_parent = get_private(el, CTX_PARENT_KEY)
        .and_then(|v| v.as_f64())
        .map(|n| ScopeId(n as u64))
        .or_else(|| enclosing_inject_parent(el));
    if let Some(parent_id) = ctx_parent {
        crate::context::set_parent(scope.id, parent_id);
    }
    apply_static_props(el, &scope);
    if scope.state.borrow().has_setup() {
        crate::scope::with_current_scope_id(scope.id, || {
            scope.state.borrow_mut().setup();
        });
    }
    let proxy = scope.into_proxy();

    // 3. Clone the template into a throwaway container to extract
    //    root attrs without touching `el` yet.
    let Some(html) = template_for(tag) else { return false };
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return false;
    };
    let sandbox = match doc.create_element("div") {
        Ok(e) => e,
        Err(_) => return false,
    };
    sandbox.set_inner_html(&html);
    let tpl_root = match first_element_child(&sandbox) {
        Some(r) => r,
        None => return false,
    };

    // 4. Verify the template is a trivial wrapper: exactly one
    //    `<slot>` child and no other element children.
    if !is_trivial_slot_wrapper(&tpl_root) {
        web_sys::console::warn_1(&JsValue::from_str(
            "pocopine: pp-as only supports trivial <slot>-wrapping templates; ignoring",
        ));
        return false;
    }

    // 5. Replace the tag's children with the user's element.
    el.set_inner_html("");
    if el.append_child(user_root.as_ref()).is_err() {
        return false;
    }

    // 6. Merge template-root attrs onto the user root per §4 of the RFC.
    merge_template_attrs_as(&tpl_root, &user_root);

    // 7. Pin scope on the user root. This is what makes `$el`,
    //    `pp-ref`, and fallthrough directives all resolve against
    //    the user's real element.
    set_private(&user_root, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
    set_private(&user_root, SCOPE_PROXY_KEY, &proxy);
    let _ = user_root.remove_attribute("pp-data");

    // 8. Fallthrough from the tag's own attrs (RFC-010).
    apply_fallthrough_attrs(el, &user_root, &scope);

    // 9. No <slot> materialisation under pp-as — the user's element
    //    IS the content. Install an empty slot store just to keep
    //    lifecycle symmetry with the normal path.
    slots::put(scope.id, SlotStore { by_name: Default::default() });

    // 10. Drop the marker so the component author's own code in the
    //     template (if they forwarded `pp-as` onto the user root, say)
    //     doesn't double-fire. Also mark the tag mounted.
    let _ = el.remove_attribute("pp-as");
    set_private(el, "__pp_mounted", &JsValue::TRUE);

    true
}

/// Walk the tag's direct children. Return `Some(el)` when exactly
/// one non-slot-template element is present among them. Named-slot
/// `<template pp-slot="…">` children are silently skipped — they
/// don't compose with `pp-as`.
fn find_single_child_element_skipping_slot_templates(tag: &Element) -> Option<Element> {
    let children = tag.child_nodes();
    let mut found: Option<Element> = None;
    for i in 0..children.length() {
        let Some(node) = children.item(i) else { continue };
        let Ok(el) = node.dyn_into::<Element>() else { continue };
        if let Some(tpl) = el.dyn_ref::<HtmlTemplateElement>() {
            if tpl.has_attribute("pp-slot") {
                continue;
            }
        }
        if found.is_some() {
            return None;
        }
        found = Some(el);
    }
    found
}

/// Template root is a trivial wrapper iff its only element child
/// is a single `<slot>`. Text / comment siblings are ignored.
fn is_trivial_slot_wrapper(tpl_root: &Element) -> bool {
    let children = tpl_root.children();
    if children.length() != 1 {
        return false;
    }
    match children.item(0) {
        Some(c) => c.local_name() == "slot",
        None => false,
    }
}

/// Copy attrs from `tpl_root` onto `user_root` per RFC-019 §4.
/// `class` / `style` join; everything else writes only when absent
/// on the user element (user wins on conflict). Internal markers
/// (`pp-data`, `pp-as`) are dropped.
///
/// RFC-020 shorthand attrs (`@event`, `:attr`) are normalised to
/// their long form (`pp-on:event`, `pp-bind:attr`) before being
/// stamped on `user_root` — `setAttribute("@click", …)` throws
/// `InvalidCharacterError` because `@` isn't a Name-start character
/// per the XML production browsers' DOM enforces. Without this
/// normalisation, every template-root `@click="handler"` and
/// every other event-shorthand silently disappeared as soon as
/// pp-as tried to forward it to the user element. Long-form attrs
/// take the same dispatch path through `bind` either way, so the
/// rewrite is invisible to authors.
fn merge_template_attrs_as(tpl_root: &Element, user_root: &Element) {
    let attrs = tpl_root.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if name == "pp-data" || name == "pp-as" {
            continue;
        }
        let val = a.value();
        let setter_name = setattr_safe_name(&name);
        match name.as_str() {
            "class" => {
                let existing = user_root.get_attribute("class").unwrap_or_default();
                let merged = merge_space(&existing, &val);
                let _ = user_root.set_attribute("class", &merged);
            }
            "style" => {
                let existing = user_root.get_attribute("style").unwrap_or_default();
                let merged = merge_semicolon(&existing, &val);
                let _ = user_root.set_attribute("style", &merged);
            }
            _ => {
                if !user_root.has_attribute(&setter_name) {
                    let _ = user_root.set_attribute(&setter_name, &val);
                }
            }
        }
    }
}

/// `setAttribute` rejects names whose first character isn't a
/// Name-start (per the XML Name production the DOM standard cites).
/// `:foo` is allowed but `@foo` isn't. Convert RFC-020 shorthands to
/// the equivalent `pp-bind:` / `pp-on:` long form so the call goes
/// through cleanly. Other names pass through unchanged.
fn setattr_safe_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix('@') {
        if !rest.is_empty() {
            return format!("pp-on:{rest}");
        }
    }
    name.to_string()
}

/// Collect the component tag's direct children into named slots,
/// ready for the walker's slot materialiser to consume. A child
/// `<template pp-slot="name" pp-let="ident">` contributes its
/// `.content` fragment to the named slot; every other child (text,
/// elements, nested templates without `pp-slot`) goes into the
/// default slot.
fn capture_slots(el: &Element) -> SlotStore {
    let doc = match web_sys::window().and_then(|w| w.document()) {
        Some(d) => d,
        None => return SlotStore { by_name: Default::default() },
    };

    // Resolve the CALLER's scope — the parent template that's
    // passing content. This scope is what the slot content's
    // directives (`@click`, `pp-text`, …) should resolve against,
    // regardless of where the slot is physically materialised
    // (inside the child's template, possibly inside a teleport).
    let (owner_scope_id, owner_proxy) = match enclosing_scope(el) {
        Some(s) => s,
        None => (ScopeId(0), JsValue::UNDEFINED),
    };

    let mut by_name: std::collections::HashMap<String, UserSlot> =
        std::collections::HashMap::new();
    let default_fragment = doc.create_document_fragment();

    let children = el.child_nodes();
    let mut to_consume: Vec<Node> = Vec::with_capacity(children.length() as usize);
    for i in 0..children.length() {
        if let Some(n) = children.item(i) {
            to_consume.push(n);
        }
    }
    for n in to_consume {
        // Named slot template?
        if let Some(tpl) = n.dyn_ref::<HtmlTemplateElement>() {
            if let Some(name) = tpl.get_attribute("pp-slot") {
                let ident = tpl.get_attribute("pp-let").unwrap_or_default();
                let source = tpl.content();
                if by_name.contains_key(&name) {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "pocopine: duplicate pp-slot=\"{name}\"; later wins"
                    )));
                }
                by_name.insert(
                    name,
                    UserSlot {
                        source,
                        ident,
                        owner_scope_id,
                        owner_proxy: owner_proxy.clone(),
                    },
                );
                continue;
            }
        }
        // Default slot.
        let _ = default_fragment.append_child(&n);
    }

    if default_fragment.child_nodes().length() > 0 {
        by_name.entry("default".to_string()).or_insert(UserSlot {
            source: default_fragment,
            // Default slot doesn't support `pp-let` scoping today;
            // scoped slots always use a named `<template pp-slot>`.
            ident: String::new(),
            owner_scope_id,
            owner_proxy,
        });
    }

    SlotStore { by_name }
}

fn apply_fallthrough_attrs(tag: &Element, root: &Element, scope: &Scope) {
    use std::collections::HashSet;

    let declared: HashSet<String> = scope
        .state
        .borrow()
        .keys()
        .iter()
        .map(|k| (*k).to_string())
        .collect();

    let attrs = tag.attributes();
    // Track which attrs the walker forwarded off the tag so they
    // can be stripped after the loop. `class` / `style` are the
    // only offenders that cause visible CSS "outer + inner" double
    // matching (same rule painting pills / borders twice). Other
    // attrs are left on the tag — devtools still sees them there,
    // and author JS (`document.querySelector('pine-foo[data-id="x"]')`)
    // keeps working.
    let mut strip_class = false;
    let mut strip_style = false;
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if name.starts_with("pp-") || name.starts_with("__pp_") {
            continue;
        }
        // RFC-020 shorthand (`@event` / `:attr`) is a directive,
        // not a plain attribute. Fallthrough would clobber the
        // template's own `@click="my_handler"` and re-bind the
        // user's handler against the child scope — where the name
        // never resolves. The handler is already bound on the tag
        // itself in the parent's scope, which is the intended
        // "event on the component" semantic. Skip.
        if name.starts_with('@') || name.starts_with(':') {
            continue;
        }
        // HTML kebab-case → Rust snake_case; matches the prop path in
        // `apply_static_props`.
        let field = name.replace('-', "_");
        if declared.contains(&field) {
            continue;
        }
        let val = a.value();
        match name.as_str() {
            "class" => {
                let existing = root.get_attribute("class").unwrap_or_default();
                let merged = merge_space(&existing, &val);
                let _ = root.set_attribute("class", &merged);
                strip_class = true;
            }
            "style" => {
                let existing = root.get_attribute("style").unwrap_or_default();
                let merged = merge_semicolon(&existing, &val);
                let _ = root.set_attribute("style", &merged);
                strip_style = true;
            }
            _ => {
                let _ = root.set_attribute(&name, &val);
            }
        }
    }
    // Strip `class` / `style` from the outer custom-element tag
    // now that they've been forwarded to the inner rendered root.
    // Without this, `.my-class { … }` author CSS matches BOTH the
    // tag and the inner element, double-painting borders /
    // padding / backgrounds. Stripping aligns pocopine with what
    // React / Vue / Svelte authors expect: one rule, one match.
    //
    // Intentionally unconditional (no debug_assertions gate) —
    // diverging CSS semantics between dev and release would
    // itself be the bigger surprise. Tests that relied on
    // `.<author-class> <descendant-tag>` selectors target the
    // inner element directly via `.<author-class>` post-strip,
    // since the inner root now owns the class uniquely.
    if strip_class {
        let _ = tag.remove_attribute("class");
    }
    if strip_style {
        let _ = tag.remove_attribute("style");
    }
}

fn merge_space(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a} {b}"),
    }
}

fn merge_semicolon(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => {
            let trimmed = a.trim_end_matches(|c: char| c.is_whitespace() || c == ';');
            format!("{trimmed}; {b}")
        }
    }
}

fn apply_static_props(el: &Element, scope: &Scope) {
    let attrs = el.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        // Skip pp-* (directives) and reserved private keys.
        if name.starts_with("pp-") || name.starts_with("__pp_") {
            continue;
        }
        // HTML attributes are kebab-case by convention (`post-id`); Rust
        // fields are snake_case (`post_id`). Map between them so authors
        // don't have to pick one side's spelling.
        let field = name.replace('-', "_");
        // RFC-031 — only `#[prop]` fields are writable by parents
        // via static HTML attributes. `#[state]` fields (the
        // default) stay opaque to the parent.
        if !scope.state.borrow().is_prop(&field) {
            continue;
        }
        let raw = a.value();
        let js = coerce_attr_value(&raw);
        scope.state.borrow_mut().set(&field, js);
    }
}

fn coerce_attr_value(raw: &str) -> JsValue {
    if raw.is_empty() {
        return JsValue::TRUE; // bare-presence attribute
    }
    if raw == "true" {
        return JsValue::TRUE;
    }
    if raw == "false" {
        return JsValue::FALSE;
    }
    let trimmed = raw.trim_start();
    let first = trimmed.as_bytes().first();
    if matches!(first, Some(b'{') | Some(b'[') | Some(b'"')) {
        if let Ok(v) = js_sys::JSON::parse(raw) {
            return v;
        }
    }
    if let Ok(n) = raw.parse::<f64>() {
        return JsValue::from_f64(n);
    }
    JsValue::from_str(raw)
}

fn first_element_child(el: &Element) -> Option<Element> {
    let children = el.children();
    children.item(0)
}

/// Expand RFC-020 shorthand prefixes. `:foo` → `pp-bind:foo`,
/// `@foo` → `pp-on:foo`. Anything else — including a bare `:` or
/// `@` with no tail — is returned unchanged so the normal pp-*
/// filter can drop it.
fn normalise_shorthand_attr(name: &str) -> String {
    if let Some(rest) = name.strip_prefix(':') {
        if !rest.is_empty() {
            return format!("pp-bind:{rest}");
        }
    }
    if let Some(rest) = name.strip_prefix('@') {
        if !rest.is_empty() {
            return format!("pp-on:{rest}");
        }
    }
    name.to_string()
}

/// Replace a `<slot>` element in a component template with the
/// matching user-provided content (from the slot store) or the
/// slot's own default children. Per RFC-011 §5.2.
fn materialize_slot(slot_el: &Element) {
    let Some(parent) = slot_el.parent_node() else { return };

    let slot_name = slot_el
        .get_attribute("name")
        .unwrap_or_else(|| "default".into());

    // Collect `:prop="path"` bindings.
    let mut bindings: Vec<(String, String)> = Vec::new();
    let attrs = slot_el.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if let Some(prop) = name.strip_prefix(':') {
            bindings.push((prop.to_string(), a.value()));
        }
    }

    // Resolve the enclosing scope. This is the component (or pp-for
    // loop) whose template contains the <slot>. Path bindings
    // resolve against this proxy.
    let (owner_scope_id, owner_proxy) = match enclosing_scope(slot_el) {
        Some(s) => s,
        None => {
            let _ = parent.remove_child(slot_el);
            return;
        }
    };

    // Walk up the scope chain to find the component that captured
    // the user's slot content. Starts at the enclosing scope and
    // climbs parent chains to handle `<slot>` inside a `pp-for`
    // body (where `enclosing_scope` returns the LoopScope, which
    // doesn't own the slot store).
    //
    // `slot_owner_*` below is the scope that *authored* the slot
    // content (the caller's template) — distinct from
    // `owner_scope_id` which is the scope the `<slot>` element
    // lives inside. Directives in slot content need to resolve
    // against the *caller* to match Vue/React conventions (and so
    // `@click="parent_handler"` works from slot content rendered
    // inside a teleported subtree).
    let (user_fragment, user_ident, slot_owner_scope, slot_owner_proxy) =
        match find_slot_with_owner(owner_scope_id, &owner_proxy, &slot_name) {
            Some((frag, ident, owner_id, owner_p)) => (Some(frag), ident, owner_id, owner_p),
            None => (None, String::new(), owner_scope_id, owner_proxy.clone()),
        };

    // Build the DocumentFragment we'll insert. User-provided content
    // wins; otherwise clone the slot's own default children.
    let doc = match web_sys::window().and_then(|w| w.document()) {
        Some(d) => d,
        None => return,
    };
    let fragment: DocumentFragment = match user_fragment {
        Some(f) => f,
        None => {
            let frag = doc.create_document_fragment();
            let kids = slot_el.child_nodes();
            for i in 0..kids.length() {
                if let Some(n) = kids.item(i) {
                    if let Ok(clone) = n.clone_node_with_deep(true) {
                        let _ = frag.append_child(&clone);
                    }
                }
            }
            frag
        }
    };

    // Move fragment's children into the parent before `slot_el`,
    // collecting the inserted Elements so we can walk + pin scope.
    let mut inserted: Vec<Element> = Vec::new();
    let frag_kids = fragment.child_nodes();
    // Grab children up front — moving into parent mutates the fragment.
    let mut frag_snapshot: Vec<Node> = Vec::with_capacity(frag_kids.length() as usize);
    for i in 0..frag_kids.length() {
        if let Some(n) = frag_kids.item(i) {
            frag_snapshot.push(n);
        }
    }
    for n in frag_snapshot {
        let _ = parent.insert_before(&n, Some(slot_el));
        if let Ok(e) = n.dyn_into::<Element>() {
            inserted.push(e);
        }
    }
    let _ = parent.remove_child(slot_el);

    // If the slot declared any :prop bindings and the user used
    // pp-let, build a SlotScope whose `owner` is the slot's
    // authoring scope so `ident.prop` / fall-through both resolve
    // against the caller. Otherwise pin the caller's scope
    // directly — without this, content in a teleported slot would
    // resolve its directives against the nearest DOM-ancestor's
    // scope (the child component), breaking
    // `@click="parent_handler"`.
    if !bindings.is_empty() && !user_ident.is_empty() {
        let slot_state = SlotScope {
            ident: user_ident,
            bindings,
            // `:prop="path"` binds evaluate in the scope that
            // *declared* the slot — that's where `path` was
            // authored, so sibling fields / magics resolve
            // correctly there.
            bind_source: owner_proxy.clone(),
            // Fall-through reads (anything not matching the
            // `pp-let` identifier) go to the *caller's* scope, so
            // `@click="parent_handler"` works from inside the
            // slot.
            caller: slot_owner_proxy.clone(),
        };
        let slot_scope = Scope::new(Rc::new(RefCell::new(slot_state)));
        // RFC-027 inject chain: the slot scope lives inside the
        // slot-*owning* component's template — children inject
        // against that component, not the caller. Directive
        // resolution still uses the caller (above) for RFC-011
        // semantics.
        crate::context::set_parent(slot_scope.id, owner_scope_id);
        let proxy = slot_scope.into_proxy();
        for el in &inserted {
            bind_borrowed_scope_to(el, slot_scope.id, &proxy);
        }
    } else {
        for el in &inserted {
            bind_borrowed_scope_to(el, slot_owner_scope, &slot_owner_proxy);
            // Stamp explicit inject-chain parent so a later
            // mount_component on this element chains to the slot
            // owner for inject, not to whatever its borrowed-DOM
            // scope happens to be.
            set_private(
                el,
                CTX_PARENT_KEY,
                &JsValue::from_f64(owner_scope_id.0 as f64),
            );
        }
    }

    for el in inserted {
        walk(&el);
    }
}

/// Walk up the scope chain starting at `start_scope_id` to find the
/// first component that captured a slot named `name`. Returns the
/// cloned fragment + user's `pp-let` ident + the slot's authoring
/// (caller's) scope if found.
fn find_slot_with_owner(
    start_scope_id: ScopeId,
    start_proxy: &JsValue,
    name: &str,
) -> Option<(DocumentFragment, String, ScopeId, JsValue)> {
    // First try the enclosing scope directly.
    if let Some(hit) = slots::lookup(start_scope_id, name) {
        return Some(hit);
    }
    // Climb LoopScope parents. `LoopScope::parent` is the outer
    // proxy; we can pull its scope id from its private SCOPE_ID on
    // the element... but at this point we don't have the element,
    // we have the proxy. Without a reverse map we walk one-at-a-time
    // via the LoopScope::parent convention: the proxy is a JS object
    // whose `$__scope_id__` isn't exposed, so we rely on
    // `Scope::all()` + ancestry lookup — punt that to a follow-up
    // RFC. For v0, the owner is always the enclosing scope; if a
    // `<slot>` appears inside a `pp-for` body, the LoopScope's
    // parent proxy is the component's proxy, and we try that next.
    let parent_proxy = Reflect::get(start_proxy, &JsValue::from_str("$__parent__"))
        .ok()
        .filter(|v| !v.is_undefined() && !v.is_null());
    if let Some(_pp) = parent_proxy {
        // Reserved for when we plumb a scope-id back through the
        // parent proxy; today we return None and rely on the
        // `<slot>` being directly inside the component template.
    }
    None
}

/// If `el` is a registered-component tag with its scope mounted on the
/// template root, return the child component's proxy so directives like
/// `pp-bind:` can write props directly.
pub fn child_component_proxy(el: &Element) -> Option<JsValue> {
    child_component_scope(el).map(|(_, p)| p)
}

/// RFC-031 — like [`child_component_proxy`] but also returns the
/// child's scope id, so callers can consult `is_prop` on the
/// child's `ComponentState` before writing through the proxy.
/// Writes via `Reflect::set(&proxy, …)` always succeed at the JS
/// layer; the is_prop gate lives at the directive site, not on
/// the proxy itself (the proxy also sees the child's OWN internal
/// writes, which must always land).
pub fn child_component_scope(el: &Element) -> Option<(ScopeId, JsValue)> {
    if !is_registered(&el.local_name()) {
        return None;
    }
    let root = first_element_child(el)?;
    scope_of_element(&root)
}

fn dispatch(el: &Element, proxy: &JsValue, scope_id: ScopeId, name: &str, value: &str) {
    let Some((dname, arg, modifiers)) = parse_attr(name) else { return };
    let Some(handler) = lookup(&dname) else { return };
    let call = DirectiveCall {
        el,
        proxy,
        scope_id,
        arg,
        modifiers,
        value: value.to_string(),
    };
    handler(&call);
}

/// Climb the parent chain until we find an element with a bound scope.
pub fn enclosing_scope(el: &Element) -> Option<(ScopeId, JsValue)> {
    let mut cur: Option<Element> = Some(el.clone());
    while let Some(e) = cur {
        if let Some(id_num) = get_private(&e, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            if let Some(proxy) = get_private(&e, SCOPE_PROXY_KEY) {
                return Some((ScopeId(id_num as u64), proxy));
            }
        }
        cur = e.parent_element();
    }
    None
}

/// Walk the DOM ancestor chain looking for the nearest scope that
/// should own `el` for RFC-027 `inject` purposes. Prefers
/// `CTX_PARENT_KEY` (stamped by slot materialisation to point at the
/// slot *owner* — the component whose template contains the `<slot>`)
/// over `SCOPE_ID_KEY` (which slot materialisation binds to the
/// *author* scope so caller-side directive resolution still works).
///
/// Without this preference, a deeply nested tag inside slotted
/// content — e.g. `<pine-dialog-close>` inside a `<div class="row">`
/// inside Content's slot — falls through to the nearest ancestor
/// with `SCOPE_ID_KEY` (the slot author's scope) and misses the
/// compound's provide/inject chain entirely.
fn enclosing_inject_parent(el: &Element) -> Option<ScopeId> {
    let mut cur: Option<Element> = el.parent_element();
    while let Some(e) = cur {
        if let Some(id) = get_private(&e, CTX_PARENT_KEY).and_then(|v| v.as_f64()) {
            return Some(ScopeId(id as u64));
        }
        if let Some(id) = get_private(&e, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            return Some(ScopeId(id as u64));
        }
        cur = e.parent_element();
    }
    None
}

/// If `el` itself owns a scope (i.e. it's a component root), return it.
/// Used by directives (e.g. `pp-bind:`) that need to decide whether they're
/// writing to an HTML attribute or to a child-component prop.
pub fn scope_of_element(el: &Element) -> Option<(ScopeId, JsValue)> {
    let id_num = get_private(el, SCOPE_ID_KEY).and_then(|v| v.as_f64())?;
    let proxy = get_private(el, SCOPE_PROXY_KEY)?;
    Some((ScopeId(id_num as u64), proxy))
}

/// Find the DOM element that has `scope_id` pinned onto it. Walks
/// from `<body>` downward — O(n) in the number of elements, fine for
/// devtools hover lookups but not for hot paths.
pub fn find_element_for_scope(scope_id: ScopeId) -> Option<Element> {
    let body = web_sys::window()?.document()?.body()?;
    let root: Element = body.into();
    find_in_subtree(&root, scope_id)
}

fn find_in_subtree(root: &Element, scope_id: ScopeId) -> Option<Element> {
    if let Some(id_num) = get_private(root, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
        if id_num as u64 == scope_id.0 {
            return Some(root.clone());
        }
    }
    let children = root.children();
    for i in 0..children.length() {
        if let Some(child) = children.item(i) {
            if let Some(found) = find_in_subtree(&child, scope_id) {
                return Some(found);
            }
        }
    }
    None
}

/// Attach an effect id to an element so it can be released on unmount.
pub fn track_effect_on(el: &Element, id: EffectId) {
    let list = match get_private(el, EFFECTS_KEY) {
        Some(v) if v.is_object() => v.dyn_into::<Array>().ok(),
        _ => None,
    }
    .unwrap_or_else(Array::new);
    list.push(&JsValue::from_f64(id.0 as f64));
    set_private(el, EFFECTS_KEY, &list);
}

// ── Element-scoped listener side-table ────────────────────────────
//
// `pp-on` / `pp-model` previously called `closure.forget()`, which
// leaks the Rust `Box<dyn FnMut>` for the listener's lifetime AND —
// for `.window` / `.document` / `.outside` variants whose target is
// not the element itself — keeps the listener firing past unmount.
//
// The fix: every listener the runtime registers goes through
// `track_listener_on`. That stashes the `(target, event, capture,
// closure)` tuple in a thread-local table keyed by a numeric id
// stamped on the element via the existing `set_private` path.
// `release_subtree` walks the ids, calls
// `remove_event_listener_with_callback` for each, and drops the
// `Closure` — which drops the underlying `Box<dyn FnMut>`.

/// One installed listener. Kept alive by the side-table so the
/// closure's JS function pointer stays valid; torn down when the
/// owning element unmounts.
struct ListenerEntry {
    target: EventTarget,
    event: String,
    capture: bool,
    closure: Closure<dyn FnMut(Event)>,
}

thread_local! {
    /// Monotonically-increasing id stamped on each element that
    /// tracks listeners. Same shape as the per-scope id stamp —
    /// cheap integer in a JS private field, rich state side-tabled.
    static LISTENER_NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static LISTENERS: RefCell<HashMap<u64, Vec<ListenerEntry>>> =
        RefCell::new(HashMap::new());
}

fn listener_slot_for(el: &Element) -> u64 {
    if let Some(v) = get_private(el, LISTENERS_KEY).and_then(|v| v.as_f64()) {
        return v as u64;
    }
    let id = LISTENER_NEXT_ID.with(|c| {
        let id = c.get();
        c.set(id + 1);
        id
    });
    set_private(el, LISTENERS_KEY, &JsValue::from_f64(id as f64));
    id
}

/// Install `closure` as an event listener for `event` on `target`,
/// and tie its lifetime to `el`. When `el`'s subtree is released,
/// `remove_event_listener_with_callback` runs and the closure's
/// `Box<dyn FnMut>` is dropped.
///
/// Use this instead of `add_event_listener_with_callback` +
/// `closure.forget()` anywhere the listener should NOT outlive the
/// element — which is every listener we register. `target` may be
/// the element itself, `window`, or `document`.
pub fn track_listener_on(
    el: &Element,
    target: EventTarget,
    event: &str,
    capture: bool,
    closure: Closure<dyn FnMut(Event)>,
) {
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_capture(capture);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        event,
        closure.as_ref().unchecked_ref(),
        &opts,
    );
    let slot = listener_slot_for(el);
    LISTENERS.with(|m| {
        m.borrow_mut()
            .entry(slot)
            .or_default()
            .push(ListenerEntry {
                target,
                event: event.to_string(),
                capture,
                closure,
            });
    });
}

/// Same as [`track_listener_on`] but passes through extra
/// `AddEventListenerOptions` (currently only `once`). Kept separate
/// so the common path stays simple. A `once` listener still needs
/// cleanup in case the element unmounts before the event fires.
pub fn track_listener_on_with_opts(
    el: &Element,
    target: EventTarget,
    event: &str,
    opts: &web_sys::AddEventListenerOptions,
    closure: Closure<dyn FnMut(Event)>,
) {
    // Opts are applied directly on the add — we retain only the
    // `capture` flag on our side because that's all removal needs
    // to match.
    let capture = opts.get_capture().unwrap_or(false);
    let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
        event,
        closure.as_ref().unchecked_ref(),
        opts,
    );
    let slot = listener_slot_for(el);
    LISTENERS.with(|m| {
        m.borrow_mut()
            .entry(slot)
            .or_default()
            .push(ListenerEntry {
                target,
                event: event.to_string(),
                capture,
                closure,
            });
    });
}

fn release_listeners(el: &Element) {
    let Some(slot) = get_private(el, LISTENERS_KEY).and_then(|v| v.as_f64()) else {
        return;
    };
    let entries = LISTENERS.with(|m| m.borrow_mut().remove(&(slot as u64)));
    if let Some(entries) = entries {
        for e in entries {
            // Removal needs the same (event, callback, capture)
            // triple that `add` received. Boolean removal form —
            // `AddEventListenerOptions` has no matching boolean-
            // returning removal call in web-sys.
            let _ = e.target.remove_event_listener_with_callback_and_bool(
                &e.event,
                e.closure.as_ref().unchecked_ref(),
                e.capture,
            );
            // `e.closure` drops here, reclaiming the Rust box.
            drop(e);
        }
    }
}

/// Count of listener entries currently retained by the
/// element-scoped listener table. Used by tests (assert
/// `release_subtree` reclaims everything) and by the devtools
/// memory-health panel (leak-over-time sparkline). Gated on
/// debug builds OR the `devtools` feature so opt-in release
/// devtools gets real numbers.
#[cfg(any(debug_assertions, feature = "devtools"))]
pub fn listener_count() -> usize {
    LISTENERS.with(|m| m.borrow().values().map(|v| v.len()).sum())
}

pub(crate) fn release_subtree(node: &Node) {
    // Recurse through children first so leaves are cleaned before roots.
    if let Ok(el) = node.clone().dyn_into::<Element>() {
        let children = el.children();
        for i in 0..children.length() {
            if let Some(c) = children.item(i) {
                release_subtree(&c);
            }
        }
        if let Some(v) = get_private(&el, EFFECTS_KEY) {
            if let Ok(arr) = v.dyn_into::<Array>() {
                for i in 0..arr.length() {
                    if let Some(n) = arr.get(i).as_f64() {
                        release(EffectId(n as u64));
                    }
                }
            }
        }
        if let Some(id) = get_private(&el, SCOPE_ID_KEY).and_then(|v| v.as_f64()) {
            // Borrowed scopes belong to some other element — don't
            // evict them from the registry when a borrower unmounts.
            let borrowed = get_private(&el, SCOPE_BORROWED_KEY)
                .map(|v| v.is_truthy())
                .unwrap_or(false);
            if !borrowed {
                let scope_id = ScopeId(id as u64);
                // Fire the `on_unmount` lifecycle hook while the scope
                // and its state are still valid.
                if let Some(scope) = Scope::find(scope_id) {
                    crate::scope::with_current_scope_id(scope_id, || {
                        scope.state.borrow_mut().unmount();
                    });
                }
                Scope::remove(scope_id);
                // RFC-032 — drop any MountEpoch entry for this scope
                // so a recycled scope id doesn't inherit stale values.
                crate::lifecycle::__clear_mount_epoch(scope_id);
            }
        }
        crate::directives::transition::release(&el);
        crate::directives::teleport::release(&el);
        crate::directives::resize::release(&el);
        crate::directives::intersect::release(&el);
        crate::directives::anchor::release(&el);
        crate::directives::roving::release(&el);
        release_listeners(&el);
    }
}

fn install_observer(root: &Element) {
    let cb = Closure::wrap(Box::new(move |records: JsValue, _obs: JsValue| {
        let Ok(arr) = records.dyn_into::<Array>() else { return };
        // Re-read every callback — devtools mounts after `start`, so
        // the panel root isn't present when the observer installs.
        let panel_root = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("__pp_devtools_root"));

        for i in 0..arr.length() {
            let Ok(rec) = arr.get(i).dyn_into::<MutationRecord>() else { continue };

            // Devtools replaces its own innerHTML on a 200ms poll.
            // Those records aren't app state — skip the whole record
            // when its target is the panel (or inside it) so we don't
            // release_subtree + re-walk the whole panel every tick.
            if let (Some(panel), Some(target)) = (panel_root.as_ref(), rec.target()) {
                if panel.contains(Some(&target)) {
                    continue;
                }
            }

            // "Removed" records report nodes detached from *this*
            // parent. When we reparent an element (a keyed `pp-for`
            // reorder, `pp-teleport`, anything similar), it still ends
            // up connected to the document — the DOM just reports the
            // detach-then-attach as separate records. Those must not
            // tear down the element's scope; only nodes that are
            // genuinely gone should release.
            let removed: NodeList = rec.removed_nodes();
            for j in 0..removed.length() {
                if let Some(n) = removed.get(j) {
                    if n.is_connected() {
                        continue;
                    }
                    release_subtree(&n);
                }
            }

            // Symmetric for "added" — anything we already walked
            // (including the element on the other end of a reparent)
            // carries WALKED_KEY. Re-walking it would create duplicate
            // effects subscribed to the same deps.
            let added: NodeList = rec.added_nodes();
            for j in 0..added.length() {
                if let Some(n) = added.get(j) {
                    if let Ok(e) = n.dyn_into::<Element>() {
                        if get_private(&e, WALKED_KEY).is_some() {
                            continue;
                        }
                        walk(&e);
                    }
                }
            }
        }
    }) as Box<dyn FnMut(JsValue, JsValue)>);

    let Ok(obs) = MutationObserver::new(cb.as_ref().unchecked_ref()) else {
        return;
    };
    let init = MutationObserverInit::new();
    init.set_child_list(true);
    init.set_subtree(true);
    let _ = obs.observe_with_options(root, &init);
    cb.forget();
}

fn set_private(el: &Element, key: &str, value: &JsValue) {
    let _ = Reflect::set(el.as_ref(), &key.into(), value);
}

fn get_private(el: &Element, key: &str) -> Option<JsValue> {
    Reflect::get(el.as_ref(), &key.into())
        .ok()
        .filter(|v| !v.is_undefined())
}
