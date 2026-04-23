//! `pp-for="item in items"` — iterate an array, clone the host
//! `<template>`'s body once per item, bind each clone against a
//! [`crate::loop_scope::LoopScope`].
//!
//! Requires the host to be a `<template>` element. The content of
//! that template is cloned per iteration; the original template stays
//! in the DOM as a mount anchor. Clones are inserted as siblings
//! before the template.
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

use js_sys::{Array, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, Element, HtmlTemplateElement, Node};

use super::DirectiveCall;
use crate::loop_scope::LoopScope;
use crate::path::resolve_path;
use crate::reactive::{effect, trigger_scope, ScopeId};
use crate::scope::Scope;
use crate::walker::{self, bind_scope_to, track_effect_on};

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

pub fn run(call: &DirectiveCall) {
    let Some((item_name, items_expr)) = parse_expr(&call.value) else {
        console::error_1(&JsValue::from_str(&format!(
            "pp-for: expected `<ident> in <path>`, got {:?}",
            call.value
        )));
        return;
    };

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
    let parent_scope_id = call.scope_id;
    let template_el: Element = call.el.clone();
    let key_expr = template_el.get_attribute("pp-key");

    let effect_id = match key_expr {
        Some(key) if !key.trim().is_empty() => run_keyed(
            item_name,
            items_expr,
            key,
            parent_proxy,
            parent_scope_id,
            template,
            template_el,
        ),
        _ => run_naive(
            item_name,
            items_expr,
            parent_proxy,
            parent_scope_id,
            template,
            template_el,
        ),
    };

    track_effect_on(call.el, effect_id);
}

/// Whole-rebuild iteration (no `pp-key`). Keeps the original
/// RFC-004 semantics.
fn run_naive(
    item_name: String,
    items_expr: String,
    parent_proxy: JsValue,
    parent_scope_id: ScopeId,
    template: HtmlTemplateElement,
    template_el: Element,
) -> crate::reactive::EffectId {
    let item_name: Rc<str> = item_name.into();
    let prior: Rc<RefCell<Vec<Element>>> = Rc::new(RefCell::new(Vec::new()));

    effect(move || {
        let items_js = resolve_path(&parent_proxy, &items_expr);
        let arr: Array = items_js.dyn_into::<Array>().unwrap_or_else(|_| Array::new());
        let total = arr.length() as usize;

        {
            let mut prior = prior.borrow_mut();
            for el in prior.drain(..) {
                let el_cap = el.clone();
                crate::directives::transition::leave_subtree(&el, move || {
                    if let Some(parent) = el_cap.parent_node() {
                        let _ = parent.remove_child(&el_cap);
                    }
                });
            }
        }
        if total == 0 {
            return;
        }

        let Some(parent_node) = template_el.parent_node() else { return };

        let mut fresh: Vec<Element> = Vec::with_capacity(total);
        for i in 0..total {
            let item = arr.get(i as u32);
            let loop_state = LoopScope {
                item_name: Rc::clone(&item_name),
                item,
                index: i,
                total,
                parent: parent_proxy.clone(),
                parent_scope_id,
            };
            let scope = Scope::new(Rc::new(RefCell::new(loop_state)));
            crate::context::set_parent(scope.id, parent_scope_id);
            let proxy = scope.into_proxy();

            let Some(clone_root) = clone_template_body(&template) else {
                console::error_1(&JsValue::from_str(
                    "pp-for: <template> body must contain exactly one element",
                ));
                break;
            };
            bind_scope_to(&clone_root, scope.id, &proxy);

            if parent_node
                .insert_before(clone_root.as_ref(), Some(template_el.as_ref()))
                .is_ok()
            {
                walker::walk(&clone_root);
                let clone_for_enter = clone_root.clone();
                crate::directives::transition::enter_subtree(&clone_for_enter, || {});
                fresh.push(clone_root);
            }
        }

        *prior.borrow_mut() = fresh;
    })
}

/// One previously-rendered clone. `loop_state` lets us mutate the
/// `LoopScope` in place on reuse without serializing through JS.
///
/// The key is shared between `PrevItem`, the pool lookup, and the
/// dedup-tracking set via `Rc<str>` — one allocation per unique
/// key per reconcile instead of two (the HashSet insert used to
/// clone a fresh `String`).
struct PrevItem {
    element: Element,
    scope_id: ScopeId,
    loop_state: Rc<RefCell<LoopScope>>,
    key: Rc<str>,
}

/// Keyed iteration. Reuses clones whose keys still appear, fires
/// `trigger_scope` so their bindings re-evaluate against the updated
/// `LoopScope`, and reorders the DOM to match the new order.
fn run_keyed(
    item_name: String,
    items_expr: String,
    key_expr: String,
    parent_proxy: JsValue,
    parent_scope_id: ScopeId,
    template: HtmlTemplateElement,
    template_el: Element,
) -> crate::reactive::EffectId {
    let item_name: Rc<str> = item_name.into();
    let prior: Rc<RefCell<Vec<PrevItem>>> = Rc::new(RefCell::new(Vec::new()));
    // Pool + seen-keys set carry allocated capacity across effect
    // re-runs. Both are fully drained at the end of each run so
    // reusing them keeps rehash / grow costs out of the hot path
    // for long-lived lists (N items reconciled K times allocates
    // once, not K times).
    let pool_cell: Rc<RefCell<HashMap<Rc<str>, PrevItem>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let seen_cell: Rc<RefCell<HashSet<Rc<str>>>> = Rc::new(RefCell::new(HashSet::new()));

    effect(move || {
        let items_js = resolve_path(&parent_proxy, &items_expr);
        let arr: Array = items_js.dyn_into::<Array>().unwrap_or_else(|_| Array::new());
        let total = arr.length() as usize;

        let Some(parent_node) = template_el.parent_node() else {
            // Template not attached — clear any tracking.
            prior.borrow_mut().clear();
            return;
        };
        let parent_node_ref: &Node = parent_node.as_ref();

        // Drain prior into a key → entry map so we can look up reuse
        // candidates in O(1).
        let mut pool = pool_cell.borrow_mut();
        pool.clear();
        let pool_cap = pool.capacity();
        if pool_cap < total {
            pool.reserve(total - pool_cap);
        }
        let old_prior: Vec<PrevItem> = {
            let mut b = prior.borrow_mut();
            std::mem::take(&mut *b)
        };
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

        let mut fresh: Vec<PrevItem> = Vec::with_capacity(total);

        for i in 0..total {
            let item = arr.get(i as u32);
            let key_val = resolve_key(&item_name, &item, i, &parent_proxy, &key_expr);
            let raw_key: Rc<str> = stringify_key(&key_val).into();

            // Make sure duplicate keys in one pass don't collapse
            // onto the first clone — the second (and later) hit gets
            // disambiguated and warned.
            let key: Rc<str> = if seen.insert(raw_key.clone()) {
                raw_key
            } else {
                console::warn_1(&JsValue::from_str(&format!(
                    "pp-for: duplicate pp-key {:?} at index {i}; treating as new",
                    &*raw_key
                )));
                let dup: Rc<str> = format!("{}__dup_{i}", &*raw_key).into();
                seen.insert(dup.clone());
                dup
            };

            if let Some(entry) = pool.remove(&key) {
                // Reuse. Update the loop state in place; fire
                // trigger_scope so effects bound to this loop re-run.
                {
                    let mut st = entry.loop_state.borrow_mut();
                    st.item = item;
                    st.index = i;
                    st.total = total;
                }
                trigger_scope(entry.scope_id);
                fresh.push(entry);
            } else {
                // New. Fresh loop scope + clone.
                let loop_rc = Rc::new(RefCell::new(LoopScope {
                    item_name: Rc::clone(&item_name),
                    item,
                    index: i,
                    total,
                    parent: parent_proxy.clone(),
                    parent_scope_id,
                }));
                let scope = Scope::new(loop_rc.clone());
                crate::context::set_parent(scope.id, parent_scope_id);
                let proxy = scope.into_proxy();

                let Some(clone_root) = clone_template_body(&template) else {
                    console::error_1(&JsValue::from_str(
                        "pp-for: <template> body must contain exactly one element",
                    ));
                    Scope::remove(scope.id);
                    break;
                };
                bind_scope_to(&clone_root, scope.id, &proxy);
                fresh.push(PrevItem {
                    element: clone_root,
                    scope_id: scope.id,
                    loop_state: loop_rc,
                    key,
                });
            }
        }

        // Anything left in the pool is no longer present — leave +
        // remove. RFC-038: route through `transition::leave_subtree`
        // so any animated descendants (Pine compounds with
        // `transition = "..."`) play their leave keyframes before
        // the actual remove_child fires. The MutationObserver-driven
        // `release_subtree` runs after the removal as before.
        for (_, entry) in pool.drain() {
            let el = entry.element.clone();
            let el_cap = el.clone();
            crate::directives::transition::leave_subtree(&el, move || {
                if let Some(parent) = el_cap.parent_node() {
                    let _ = parent.remove_child(&el_cap);
                }
            });
        }

        // RFC-038 — FLIP prep: snapshot client rects for every
        // reused clone BEFORE the insert_before loop below moves
        // them. We check `data-pp-animate="flip"` to skip scopes
        // that don't opt in (no motion tax on normal lists).
        //
        // For Pine compounds the clone_root is an outer custom
        // element (`<pine-tags-input-item>`) with no display box —
        // `getBoundingClientRect` on it returns zero. The visible
        // layout box lives on the inner rendered root (the first
        // element child). Snapshot + animate that.
        //
        // All clones descend from the same template, so if the
        // first reused clone doesn't carry `data-pp-animate="flip"`
        // none of them will. Cheap probe lets non-animated lists
        // skip the whole N-item snapshot walk + `get_bounding_client_rect`
        // per item (each one is a JS crossing + layout read).
        let flip_opted_in = fresh.iter().any(|entry| {
            entry.element.parent_node().is_some()
                && entry.element.get_attribute("data-pp-animate").as_deref() == Some("flip")
        });
        let mut flip_snapshots: HashMap<Rc<str>, (Element, web_sys::DomRect)> = HashMap::new();
        if flip_opted_in {
            for entry in &fresh {
                if entry.element.parent_node().is_none()
                    || entry.element.get_attribute("data-pp-animate").as_deref() != Some("flip")
                {
                    continue;
                }
                let target =
                    first_layout_child(&entry.element).unwrap_or_else(|| entry.element.clone());
                flip_snapshots
                    .insert(entry.key.clone(), (target.clone(), target.get_bounding_client_rect()));
            }
        }

        // Reorder + walk new clones. See issue #4 for motivation —
        // an unconditional `insert_before` blurs focused descendants
        // on every reactive flush because each move is spec-defined
        // as remove + re-insert.
        //
        // Short-circuit: if the DOM already matches the new order
        // (every `fresh[i]` is parented at `parent_node` AND its
        // next sibling is `fresh[i+1]` or `template_el` for the
        // last slot), there's nothing to do and we skip the entire
        // reorder pass. Otherwise fall through to an unconditional
        // insert loop — per-iter "skip if in place" is locally
        // correct but globally wrong during a reorder (can leave
        // elements stranded in their old positions).
        let already_ordered = fresh.iter().enumerate().all(|(i, entry)| {
            let correct_parent = entry
                .element
                .parent_node()
                .map(|p| p.is_same_node(Some(parent_node_ref)))
                .unwrap_or(false);
            let expected_next: &Node = if i + 1 < fresh.len() {
                fresh[i + 1].element.as_ref()
            } else {
                template_el.as_ref()
            };
            let correct_next = entry
                .element
                .next_sibling()
                .map(|n| n.is_same_node(Some(expected_next)))
                .unwrap_or(false);
            correct_parent && correct_next
        });

        let mut newly_walked: Vec<Element> = Vec::new();
        if !already_ordered {
            for entry in &fresh {
                let was_in_place = entry
                    .element
                    .parent_node()
                    .map(|p| p.is_same_node(Some(parent_node_ref)))
                    .unwrap_or(false);
                let _ = parent_node.insert_before(
                    entry.element.as_ref(),
                    Some(template_el.as_ref()),
                );
                if !was_in_place {
                    newly_walked.push(entry.element.clone());
                }
            }
        }
        // Walk freshly-inserted clones AFTER they're in the tree so
        // directive setup can look up the enclosing scope via parent
        // chain if it needs to.
        for el in &newly_walked {
            walker::walk(el);
        }
        // RFC-038 — fire enter on each newly-walked clone subtree
        // so a freshly-added TagsInput chip / DropdownMenu Item /
        // etc. plays its mount preset.
        for el in newly_walked {
            crate::directives::transition::enter_subtree(&el, || {});
        }

        // RFC-038 — FLIP play phase. For each reused clone that was
        // snapshotted, kick off the inverse-transform-to-identity
        // animation via WAAPI. Runs AFTER the walker stamps
        // `data-pp-animate` + inserts, so the elements' new
        // positions are real.
        if !flip_snapshots.is_empty() {
            for entry in &fresh {
                if let Some((target, old_rect)) = flip_snapshots.remove(&entry.key) {
                    crate::animate::flip_from_snapshot(
                        &target,
                        old_rect,
                        crate::animate::FlipOptions::default(),
                    );
                }
            }
        }

        *prior.borrow_mut() = fresh;
    })
}

/// Resolve the `pp-key` expression for one iteration without
/// creating a throw-away `Scope` + proxy. Handles the three forms
/// we actually see in practice:
///
/// * `"<item_name>"` → the raw item.
/// * `"<item_name>.<path>"` → walk `path` segments on the item via
///   `Reflect::get`. Non-tracked, matches the per-iteration read we
///   want — we don't subscribe the outer pp-for effect to any of
///   the item's own fields.
/// * `"$index"` → the current index.
/// * anything else → fall through to a normal `resolve_path` against
///   the parent proxy so keys like `"$store.selected_id"` still work.
fn resolve_key(
    item_name: &str,
    item: &JsValue,
    index: usize,
    parent_proxy: &JsValue,
    expr: &str,
) -> JsValue {
    let trimmed = expr.trim();
    if trimmed == "$index" {
        return JsValue::from_f64(index as f64);
    }
    if trimmed == item_name {
        return item.clone();
    }
    let prefix = format!("{item_name}.");
    if let Some(rest) = trimmed.strip_prefix(&prefix) {
        return rest.split('.').fold(item.clone(), |acc, segment| {
            if segment.is_empty() {
                acc
            } else {
                Reflect::get(&acc, &JsValue::from_str(segment))
                    .unwrap_or(JsValue::UNDEFINED)
            }
        });
    }
    resolve_path(parent_proxy, trimmed)
}

/// Canonicalise a key value to a string. Strings come through
/// unwrapped so adjacent hashes (`123` as number vs. string) don't
/// collide with their JSON-quoted form.
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
