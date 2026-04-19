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

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{
    DocumentFragment, Element, HtmlTemplateElement, MutationObserver, MutationObserverInit,
    MutationRecord, Node, NodeList,
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
const INIT_PENDING_KEY: &str = "__pp_init_pending";
const WALKED_KEY: &str = "__pp_walked";

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
    let has_hook = scope.state.borrow().has_on_mount();
    if !has_hook {
        return;
    }
    crate::scope::with_current_scope_id(id, || {
        scope.state.borrow_mut().mount();
    });
    crate::reactive::trigger_scope(id);
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
    let mut pp_attrs: Vec<(String, String)> = Vec::new();
    let attrs = el.attributes();
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
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
    let _ = &proxy;
    let _ = scope_id;

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
    let Some(scope) = instantiate(tag) else { return };
    // Apply static props BEFORE building the proxy so trigger doesn't fire
    // before any effect subscribes.
    apply_static_props(el, &scope);
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
    }

    // Mark the tag as mounted so the recursive walk doesn't re-enter this
    // branch if, for some reason, the walker visits it again.
    set_private(el, "__pp_mounted", &JsValue::TRUE);
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

    let mut by_name: std::collections::HashMap<String, UserSlot> =
        std::collections::HashMap::new();
    let default_fragment = doc.create_document_fragment();
    let mut default_ident = String::new();

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
                by_name.insert(name, UserSlot { source, ident });
                continue;
            }
        }
        // Default slot.
        let _ = default_fragment.append_child(&n);
    }

    if default_fragment.child_nodes().length() > 0 {
        by_name
            .entry("default".to_string())
            .or_insert(UserSlot {
                source: default_fragment,
                ident: default_ident.clone(),
            });
        // `default_ident` stays empty — default slot currently has no
        // `pp-let` concept; scoped slots always use a named template.
        let _ = &mut default_ident;
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
    for i in 0..attrs.length() {
        let Some(a) = attrs.item(i) else { continue };
        let name = a.name();
        if name.starts_with("pp-") || name.starts_with("__pp_") {
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
            }
            "style" => {
                let existing = root.get_attribute("style").unwrap_or_default();
                let merged = merge_semicolon(&existing, &val);
                let _ = root.set_attribute("style", &merged);
            }
            _ => {
                let _ = root.set_attribute(&name, &val);
            }
        }
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
    let (user_fragment, user_ident) =
        match find_slot_with_owner(owner_scope_id, &owner_proxy, &slot_name) {
            Some(pair) => (Some(pair.0), pair.1),
            None => (None, String::new()),
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
    // pp-let, build a SlotScope and pin it on each inserted root.
    // Bindings with no user pp-let still get a scope but the ident
    // is empty — the scope's `get` never matches, so content behaves
    // as an ordinary unbound slot.
    if !bindings.is_empty() && !user_ident.is_empty() {
        let slot_state = SlotScope {
            ident: user_ident,
            bindings,
            owner: owner_proxy,
        };
        let slot_scope = Scope::new(Rc::new(RefCell::new(slot_state)));
        let proxy = slot_scope.into_proxy();
        for el in &inserted {
            bind_borrowed_scope_to(el, slot_scope.id, &proxy);
        }
    }

    for el in inserted {
        walk(&el);
    }
}

/// Walk up the scope chain starting at `start_scope_id` to find the
/// first component that captured a slot named `name`. Returns the
/// cloned fragment + user's `pp-let` identifier if found.
fn find_slot_with_owner(
    start_scope_id: ScopeId,
    start_proxy: &JsValue,
    name: &str,
) -> Option<(DocumentFragment, String)> {
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
    if !is_registered(&el.local_name()) {
        return None;
    }
    let root = first_element_child(el)?;
    scope_of_element(&root).map(|(_, p)| p)
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

fn release_subtree(node: &Node) {
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
            }
        }
        crate::directives::transition::release(&el);
        crate::directives::teleport::release(&el);
        crate::directives::resize::release(&el);
        crate::directives::intersect::release(&el);
        crate::directives::anchor::release(&el);
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
