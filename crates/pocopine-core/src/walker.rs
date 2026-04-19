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

use js_sys::{Array, Reflect};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{
    Element, MutationObserver, MutationObserverInit, MutationRecord, Node, NodeList,
};

use crate::directives::{lookup, parse_attr, DirectiveCall};
use crate::reactive::{release, EffectId, ScopeId};
use crate::registry::instantiate;
use crate::scope::Scope;
use crate::templates::{is_registered, template_for};

const SCOPE_ID_KEY: &str = "__pp_scope_id";
const SCOPE_PROXY_KEY: &str = "__pp_scope_proxy";
const SCOPE_BORROWED_KEY: &str = "__pp_scope_borrowed";
const EFFECTS_KEY: &str = "__pp_effects";

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

/// Pre-order walk: bind this element, then recurse into its children.
/// Public so the router can walk the custom-element tag it creates
/// inside an `<pp-outlet>`.
pub fn walk(el: &Element) {
    bind(el);
    let children = el.children();
    for i in 0..children.length() {
        if let Some(child) = children.item(i) {
            walk(&child);
        }
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
    if is_registered(&tag) && get_private(el, SCOPE_ID_KEY).is_none()
        && get_private(el, "__pp_mounted").is_none()
    {
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

    // Second pass: everything except pp-data. `pp-init` is handled last so
    // it sees the fully-bound element.
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
        dispatch(el, &proxy, scope_id, "pp-init", &v);
    }

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
///  * move captured children into the first `<slot>` within the template.
fn mount_component(el: &Element, tag: &str) {
    let Some(scope) = instantiate(tag) else { return };
    // Apply static props BEFORE building the proxy so trigger doesn't fire
    // before any effect subscribes.
    apply_static_props(el, &scope);
    let proxy = scope.into_proxy();

    // Capture slot content (direct children of the tag before we clobber).
    let slot_content = capture_child_nodes(el);

    // Clone the registered template in.
    let Some(html) = template_for(tag) else { return };
    el.set_inner_html(&html);

    // Bind scope to the template's root element and strip pp-data so the
    // recursive walker doesn't try to re-instantiate.
    if let Some(root) = first_element_child(el) {
        set_private(&root, SCOPE_ID_KEY, &JsValue::from_f64(scope.id.0 as f64));
        set_private(&root, SCOPE_PROXY_KEY, &proxy);
        let _ = root.remove_attribute("pp-data");
    }

    // Move captured slot content into the first <slot> in the clone.
    relocate_slot_content(el, slot_content);

    // Mark the tag as mounted so the recursive walk doesn't re-enter this
    // branch if, for some reason, the walker visits it again.
    set_private(el, "__pp_mounted", &JsValue::TRUE);
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

fn capture_child_nodes(el: &Element) -> Vec<Node> {
    let list: NodeList = el.child_nodes();
    let mut out: Vec<Node> = Vec::with_capacity(list.length() as usize);
    for i in 0..list.length() {
        if let Some(n) = list.item(i) {
            out.push(n);
        }
    }
    out
}

fn first_element_child(el: &Element) -> Option<Element> {
    let children = el.children();
    children.item(0)
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

fn relocate_slot_content(el: &Element, content: Vec<Node>) {
    if content.is_empty() {
        return;
    }
    let slot = match el.query_selector("slot") {
        Ok(Some(s)) => s,
        _ => return, // No slot in template — drop the captured content.
    };
    let Some(parent) = slot.parent_node() else { return };
    for node in &content {
        let _ = parent.insert_before(node, Some(&slot));
    }
    // Remove the placeholder <slot> itself.
    let _ = parent.remove_child(&slot);
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
                Scope::remove(ScopeId(id as u64));
            }
        }
        crate::directives::transition::release(&el);
        crate::directives::teleport::release(&el);
    }
}

fn install_observer(root: &Element) {
    let cb = Closure::wrap(Box::new(move |records: JsValue, _obs: JsValue| {
        let Ok(arr) = records.dyn_into::<Array>() else { return };
        for i in 0..arr.length() {
            let Ok(rec) = arr.get(i).dyn_into::<MutationRecord>() else { continue };
            let added: NodeList = rec.added_nodes();
            for j in 0..added.length() {
                if let Some(n) = added.get(j) {
                    if let Ok(e) = n.dyn_into::<Element>() {
                        walk(&e);
                    }
                }
            }
            let removed: NodeList = rec.removed_nodes();
            for j in 0..removed.length() {
                if let Some(n) = removed.get(j) {
                    release_subtree(&n);
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
