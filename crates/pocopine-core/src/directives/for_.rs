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

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{console, DocumentFragment, Element, HtmlTemplateElement, Node};

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

fn retract_from_prior(prior: &Rc<RefCell<Vec<PrevItem>>>, key: &Rc<str>) {
    let mut p = prior.borrow_mut();
    p.retain(|item| !(Rc::ptr_eq(&item.key, key) && item.leaving));
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

/// Cheap predicate over the leave pool — true if any entry
/// has a transition in its subtree. The bulk-clear fast path
/// in `run_keyed` falls back to the per-row leave loop when
/// this returns true, so transition keyframes still play.
fn has_leavers_with_transition_entries<'a>(
    entries: impl IntoIterator<Item = &'a PrevItem>,
) -> bool {
    entries
        .into_iter()
        .any(|entry| crate::directives::transition::has_transition_in_subtree(&entry.element))
}

/// Predicate for the bulk-clear path. `replace_children_with_node_1`
/// removes every child of `parent_el` (text + comment included)
/// before reinstating `template_el`. We only take the fast path
/// when:
///
/// * The parent has exactly `pool.len() + 1` element children, and
///   every pool clone plus `template_el` is a direct child of that
///   parent — no user-authored element sibling can fit in that count.
/// * Every non-element child is a whitespace-only `Text` node
///   (typical formatting indentation between rows).
///
/// Comments, non-whitespace text, and unrecognised element
/// siblings all fall through to the per-row remove loop.
fn bulk_clear_safe<'a>(
    parent_el: &Element,
    entries: impl IntoIterator<Item = &'a PrevItem>,
    template_el: &Element,
    pool_count: usize,
) -> bool {
    let parent_node: &Node = parent_el.as_ref();
    if parent_el.child_element_count() as usize != pool_count + 1 {
        return false;
    }
    let template_parent_ok = template_el
        .parent_node()
        .as_ref()
        .is_some_and(|parent| parent.is_same_node(Some(parent_node)));
    if !template_parent_ok {
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
        match node.node_type() {
            Node::ELEMENT_NODE => {}
            Node::TEXT_NODE => {
                let text = node.text_content().unwrap_or_default();
                if !text.chars().all(|c| c.is_whitespace()) {
                    return false;
                }
            }
            // Comment, CDATA, processing instruction, etc.
            _ => return false,
        }
    }
    true
}

fn bulk_clear_compiled(parent_el: &Element, entries: &[PrevItem], template_el: &Element) -> bool {
    let pool_count = entries.len();
    if pool_count == 0
        || has_leavers_with_transition_entries(entries.iter())
        || !bulk_clear_safe(parent_el, entries.iter(), template_el, pool_count)
    {
        return false;
    }

    let scope_ids: Vec<ScopeId> = entries.iter().map(|entry| entry.scope_id).collect();
    crate::walker::mark_bulk_release(parent_el);
    crate::directives::for_plan::unmount_rows_bulk(&scope_ids);
    Scope::remove_compiled_rows(&scope_ids);
    parent_el.replace_children_with_node_1(template_el.as_ref());
    true
}

fn flip_target_for_entry(entry: &PrevItem) -> Option<Element> {
    if entry.element.parent_node().is_none()
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

    let template_el: Element = call.el.clone();
    let key_expr = template_el
        .get_attribute("pp-key")
        .filter(|k| !k.trim().is_empty());
    let stagger_ms: u32 = template_el
        .get_attribute("pp-stagger")
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0);

    install(
        template,
        call.proxy.clone(),
        call.scope_id,
        item_name,
        items_expr,
        key_expr,
        stagger_ms,
    );
}

/// Compiled-path entry point. Skips the `<template>` cast +
/// `pp-for` parse + `pp-key` / `pp-stagger` attribute reads —
/// the macro provides everything pre-resolved.
///
/// Called by `apply_static_plan` for every `StaticForPlan`
/// entry the classifier emitted. RFC-058 Phase 4.2 — extracted
/// so the runtime walker dispatch path and the plan applier
/// share one keyed/naive selection body. Coexists with the
/// RFC-054 row-plan registry: `lookup_for_template` reads
/// `data-pp-row-plan` from the template element, which the
/// template-plan classifier bakes in via the §6.2 layering
/// path.
pub fn install(
    template: HtmlTemplateElement,
    parent_proxy: JsValue,
    parent_scope_id: ScopeId,
    item_name: String,
    items_expr: String,
    key_expr: Option<String>,
    stagger_ms: u32,
) {
    let template_el: Element = template.clone().into();
    let track_anchor = template_el.clone();
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
            template,
            template_el,
            stagger_ms,
        ),
        _ => run_naive(
            item_name,
            items_expr,
            parent_proxy,
            parent_scope_id,
            template,
            template_el,
            stagger_ms,
        ),
    };

    track_effect_on(&track_anchor, effect_id);
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
    stagger_ms: u32,
) -> crate::reactive::EffectId {
    let item_name: Rc<str> = item_name.into();
    let prior: Rc<RefCell<Vec<Element>>> = Rc::new(RefCell::new(Vec::new()));

    effect(move || {
        let items_js = resolve_path(&parent_proxy, &items_expr);
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

        let Some(parent_node) = template_el.parent_node() else {
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
/// dedup-tracking set via `Rc<str>` — one allocation per unique
/// key per reconcile instead of two (the HashSet insert used to
/// clone a fresh `String`).
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
    key: Rc<str>,
    item_value: JsValue,
    item_sig: String,
    leaving: bool,
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

/// Keyed iteration. Reuses clones whose keys still appear, fires
/// `trigger_scope` so their bindings re-evaluate against the updated
/// `LoopScope`, and reorders the DOM to match the new order.
#[allow(clippy::too_many_arguments)]
fn run_keyed(
    item_name: String,
    items_expr: String,
    key_expr: String,
    parent_proxy: JsValue,
    parent_scope_id: ScopeId,
    template: HtmlTemplateElement,
    template_el: Element,
    stagger_ms: u32,
) -> crate::reactive::EffectId {
    let key_resolver = KeyResolver::parse(&item_name, &key_expr);
    // RFC 054 — opportunistic compiled-row-plan fast path. Looked
    // up once per `pp-for` directive bind (NOT per row). Templates
    // the macro flagged as eligible (flat keyed table rows, no
    // nested directives, supported binding/listener envelope) are
    // stamped with `data-pp-row-plan="<id>"`. None ⇒ generic
    // walker; Some ⇒ skip the per-row attribute scan and patch
    // dynamic nodes from the plan directly.
    let row_plan: Option<Rc<crate::directives::for_plan::CompiledRowPlan>> =
        crate::directives::for_plan::lookup_for_template(&template_el);
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
    let pool_cell: Rc<RefCell<HashMap<Rc<str>, PrevItem>>> = Rc::new(RefCell::new(HashMap::new()));
    let seen_cell: Rc<RefCell<HashSet<Rc<str>>>> = Rc::new(RefCell::new(HashSet::new()));

    effect(move || {
        let reconcile_total_start = crate::profiler::reconcile::start();
        let items_js = resolve_path(&parent_proxy, &items_expr);
        let arr: Array = items_js
            .dyn_into::<Array>()
            .unwrap_or_else(|_| Array::new());
        let total = arr.length() as usize;

        let Some(parent_node) = template_el.parent_node() else {
            // Template not attached — clear any tracking.
            prior.borrow_mut().clear();
            crate::profiler::reconcile::record_total(reconcile_total_start);
            return;
        };
        let parent_node_ref: &Node = parent_node.as_ref();
        if let Some(plan) = row_plan.as_ref() {
            crate::directives::for_plan::ensure_delegated_listeners(
                plan,
                &template_el,
                parent_node_ref,
            );
        }

        // Drain prior into a key → entry map so we can look up reuse
        // candidates in O(1).
        let pool_build_start = crate::profiler::reconcile::start();
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
        // Clearing a compiled list does not need keyed lookup at all:
        // every prior row is leaving. Try the same safe bulk teardown
        // before hashing 10K keys into `pool`; fall back to the normal
        // reconcile path if transitions or sibling structure require it.
        if total == 0 && row_plan.is_some() {
            if let Some(parent_el) = parent_node.dyn_ref::<Element>() {
                crate::profiler::reconcile::record_pool_build(pool_build_start);
                let leaver_drain_start = crate::profiler::reconcile::start();
                if bulk_clear_compiled(parent_el, &old_prior, &template_el) {
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
        for i in 0..total {
            let item = arr.get(i as u32);
            let key_val = key_resolver.resolve(&item, i, &parent_proxy);
            let raw_key: Rc<str> = stringify_key(&key_val).into();
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
                    parent: parent_proxy.clone(),
                    parent_scope_id,
                }));
                let scope = Scope::new(loop_rc.clone());
                crate::context::set_parent(scope.id, parent_scope_id);

                let clone_start = crate::profiler::mount::start();
                let Some(clone_root) = clone_template_body(&template) else {
                    console::error_1(&JsValue::from_str(
                        "pp-for: <template> body must contain exactly one element",
                    ));
                    Scope::remove(scope.id);
                    break;
                };
                crate::profiler::mount::record_clone_template_body(clone_start);
                if elide_proxy {
                    walker::bind_scope_id_only(&clone_root, scope.id);
                } else {
                    let proxy = scope.into_proxy();
                    bind_scope_to(&clone_root, scope.id, &proxy);
                }
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
        let mut leaver_flip_snapshots: HashMap<Rc<str>, (Element, web_sys::DomRect)> =
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
        // `parent_node` are our clones plus `template_el`, do
        // ONE `replaceChildren(template_el)` instead of
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
        if total == 0
            && !pool.is_empty()
            && row_plan.is_some()
            && !has_leavers_with_transition_entries(pool.values())
        {
            if let Some(parent_el) = parent_node.dyn_ref::<Element>() {
                let pool_count = pool.len();
                // Strict guard: `parent_node` owns *exactly* the
                // pool's clones plus `template_el`, and any
                // non-element children are whitespace-only
                // (typical template indentation). Comments,
                // user text, or unexpected element siblings
                // (e.g. a `<tr class="header">` next to a
                // `<template pp-for>` inside the same `<tbody>`)
                // bail out — `replace_children_with_node_1`
                // below would otherwise nuke them.
                if bulk_clear_safe(parent_el, pool.values(), &template_el, pool_count) {
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
                    crate::walker::mark_bulk_release(parent_el);
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
                    parent_el.replace_children_with_node_1(template_el.as_ref());
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
                let key_for_retract = Rc::clone(&entry.key);
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
        // next sibling is `fresh[i+1]` or `template_el` for the
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
                template_el.as_ref()
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
        let mut flip_snapshots: HashMap<Rc<str>, (Element, web_sys::DomRect)> = HashMap::new();
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
                            template_el.as_ref()
                        };
                        let correct_next = next_non_leaving(entry.element.next_sibling())
                            .map(|n| n.is_same_node(Some(expected_next)))
                            .unwrap_or(false);
                        correct_parent && correct_next
                    });
            if can_batch_suffix {
                let doc = template_el
                    .owner_document()
                    .expect("template element should belong to a document");
                let fragment: DocumentFragment = doc.create_document_fragment();
                let dom_insert_start = crate::profiler::mount::start();
                for entry in &fresh[suffix_insert_start..] {
                    let _ = fragment.append_child(entry.element.as_ref());
                    newly_walked.push(entry.element.clone());
                }
                let _ = parent_node.insert_before(fragment.as_ref(), Some(template_el.as_ref()));
                crate::profiler::mount::record_dom_insertion(dom_insert_start);
            } else {
                // Iterate back-to-front and use the next fresh entry
                // (or template_el for the last) as the insert anchor.
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
                    let anchor: &Node = if i + 1 < fresh.len() {
                        fresh[i + 1].element.as_ref()
                    } else {
                        template_el.as_ref()
                    };
                    let already_here = was_in_place
                        && next_non_leaving(entry.element.next_sibling())
                            .map(|n| n.is_same_node(Some(anchor)))
                            .unwrap_or(false);
                    if !already_here {
                        let _ = parent_node.insert_before(entry.element.as_ref(), Some(anchor));
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
        // RFC 054 — the compiled fast path swaps the generic walker
        // for a plan-driven mount: clone is already in the DOM with
        // its scope bound, so we resolve the dynamic node paths
        // and install bindings/listeners directly. Eligibility is
        // checked at macro time and the template is stamped with
        // `data-pp-row-plan="<id>"`; `lookup_for_template` returns
        // `None` for any template that didn't qualify, and we fall
        // back to the generic walker.
        for el in &newly_walked {
            // For compiled rows we already know the scope id (stamped
            // at clone time) and whether to pass a proxy: elision
            // means none was minted, so we pass `None` and
            // `mount_row_compiled` lazy-mints on first listener fire.
            // Going through `enclosing_scope` here would defeat the
            // elision by triggering the lazy-mint path immediately.
            if let Some(plan) = &row_plan {
                if let Some(sid) = walker::scope_id_of_element(el) {
                    let proxy_for_mount = if elide_proxy {
                        None
                    } else {
                        crate::walker::scope_of_element(el).map(|(_, p)| p)
                    };
                    crate::directives::for_plan::mount_row_compiled(
                        plan,
                        el,
                        sid,
                        proxy_for_mount.as_ref(),
                    );
                    walker::mark_walked(el);
                    continue;
                }
            }
            crate::profiler::mount::record_generic_row_mounted();
            walker::walk(el);
        }
        // RFC-038 — fire enter on each newly-walked clone subtree
        // so a freshly-added TagsInput chip / DropdownMenu Item /
        // etc. plays its mount preset. When the author set
        // `pp-stagger="<ms>"`, space the enters by that per-item
        // delay instead of all firing at once.
        fire_staggered_enter(&newly_walked, stagger_ms);

        // RFC-038 — FLIP play phase. Runs AFTER the walker stamps
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
    /// `item.a.b.c` — pre-split `[a, b, c]` so we walk
    /// `Reflect::get` per segment without re-splitting per item.
    ItemPath(Vec<String>),
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
            return Self::ItemPath(
                rest.split('.')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
        }
        Self::External(trimmed.to_string())
    }

    fn resolve(&self, item: &JsValue, index: usize, parent_proxy: &JsValue) -> JsValue {
        match self {
            Self::Index => JsValue::from_f64(index as f64),
            Self::Item => item.clone(),
            Self::ItemPath(segments) => segments.iter().fold(item.clone(), |acc, segment| {
                Reflect::get(&acc, &JsValue::from_str(segment)).unwrap_or(JsValue::UNDEFINED)
            }),
            Self::External(path) => resolve_path(parent_proxy, path),
        }
    }
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
