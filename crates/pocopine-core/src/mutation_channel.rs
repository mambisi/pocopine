//! RFC-095 W4 — batched DOM-mutation channel for compiled row
//! mounts.
//!
//! The profiler pins the remaining `runLots(10000)` gap to
//! per-row wasm↔JS bridge crossings: clone, node-path walks,
//! text/class writes, scope stamps, and fragment appends each
//! cross the boundary once per row (~10 crossings × 10K rows).
//! This module collapses a whole batch of row mounts into ONE
//! crossing: the row plan is registered once as a JS descriptor,
//! and [`mount_rows`] hands the interpreter the prototype, the
//! anchor, the live items array (already a JS value), and the
//! freshly minted scope ids — the interpreter loops natively,
//! reading `items[i]` itself, and returns every node handle the
//! Rust side needs (row roots, binding nodes, listener nodes) in
//! one flat array.
//!
//! Scope: keyed `pp-for` sites with a proxy-elided
//! [`CompiledRowPlan`](crate::directives::for_plan::CompiledRowPlan)
//! — the RFC-054 fast path, which is exactly where the benchmark
//! cost lives. Everything else keeps the direct web-sys path.
//! The W0 differential harness exercises both modes (see
//! [`set_enabled`]).
//!
//! Design note vs the RFC's sledgehammer sketch: no byte buffer.
//! Because the items array lives JS-side and the op sequence per
//! plan is static, a per-plan *descriptor* (data, registered
//! once) plus one rich call per batch carries strictly less
//! per-row information than an op stream would — and there is no
//! encoder/decoder pair to drift. The buffer generalisation only
//! becomes necessary if heterogeneous op streams (update-path
//! batching) land later.

use std::cell::Cell;

use js_sys::{Array, Object, Reflect};
use wasm_bindgen::prelude::*;
use web_sys::Element;

use crate::directives::for_plan::BindingKind;

thread_local! {
    /// Runtime toggle. Defaults to ON; tests flip it to cover the
    /// direct path, and a page can opt out before mount via
    /// `window.__POCOPINE_MUTATION_CHANNEL = false`.
    static ENABLED: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Whether channel mounts are active. First call resolves the
/// page-level override; later calls are a plain `Cell` read.
pub fn enabled() -> bool {
    ENABLED.with(|e| match e.get() {
        Some(v) => v,
        None => {
            let v = match web_sys::window()
                .and_then(|w| Reflect::get(&w, &"__POCOPINE_MUTATION_CHANNEL".into()).ok())
            {
                Some(v) if !v.is_undefined() => v.is_truthy(),
                _ => true,
            };
            e.set(Some(v));
            v
        }
    })
}

/// Force the toggle (tests / differential harness).
pub fn set_enabled(v: bool) {
    ENABLED.with(|e| e.set(Some(v)));
}

#[wasm_bindgen(inline_js = r#"
let plans = [];

export function pp_chan_register_plan(desc) {
    plans.push(desc);
    return plans.length - 1;
}

function walk(root, path) {
    let n = root;
    for (let k = 0; k < path.length; k++) n = n.children[path[k]];
    return n;
}

function walkChecked(root, path) {
    let n = root;
    for (let k = 0; k < path.length; k++) {
        n = n.children[path[k]];
        if (!n) return null;
    }
    return n;
}

// Mirrors for_plan.rs::js_to_string (whose number branch now
// defers to JS String() semantics — see js_number_string).
function toText(v) {
    if (v === null || v === undefined) return "";
    const t = typeof v;
    if (t === "string") return v;
    if (t === "number" || t === "boolean") return String(v);
    try {
        const s = JSON.stringify(v);
        return s === undefined ? "" : s;
    } catch (_e) {
        // Circular / throwing toJSON — match the Rust side's ""
        // for unstringifiable values instead of aborting the
        // whole batch through the wasm import.
        return "";
    }
}

// Mirrors for_plan.rs::serialise_class_value + apply_binding's
// Class arm: null/undefined/false clear, strings pass through,
// objects join their truthy keys, anything else is left alone.
function applyClass(node, v) {
    let s;
    if (v === null || v === undefined || v === false) s = "";
    else if (typeof v === "string") s = v;
    else if (typeof v === "object") {
        // Own enumerable keys only — parity with the Rust side's
        // js_sys::Object::keys (for-in would also walk inherited
        // prototype additions).
        const parts = [];
        for (const k of Object.keys(v)) if (v[k]) parts.push(k);
        s = parts.join(" ");
    } else return;
    if (s === "") node.removeAttribute("class");
    else node.setAttribute("class", s);
}

// Mirrors for_plan.rs::evaluate_fast_path for the item-rooted
// subset (parent-rooted bindings are descriptor-marked `skip`
// and patched by the list watcher after mount, as on the direct
// path). roots: 0 = item, 1 = $index, 2 = $first, 3 = $last.
function evalPath(p, item, i, total, parentVals) {
    let cur;
    switch (p.root) {
        case 0: cur = item; break;
        case 1: cur = i; break;
        case 2: cur = i === 0; break;
        case 3: cur = i + 1 === total; break;
        // Parent-rooted: row-invariant, resolved Rust-side per
        // flush; keys[0] indexes parentVals.
        case 4: return parentVals[p.keys[0]];
    }
    const keys = p.keys;
    for (let k = 0; k < keys.length; k++) {
        // Reflect.get throws on primitives (the Rust side maps
        // that to UNDEFINED); plain `cur[key]` would auto-box
        // ("abc".length === 3) and diverge from the direct path.
        if (cur === null || typeof cur !== "object") return undefined;
        cur = cur[keys[k]];
    }
    return cur;
}

function evalExpr(e, item, i, total, parentVals) {
    if (e.t === 0) return evalPath(e.p, item, i, total, parentVals);
    // TernaryEq — Object.is(lhs, rhs) ^ invert ? then : else.
    const eq = Object.is(
        evalPath(e.l, item, i, total, parentVals),
        evalPath(e.r, item, i, total, parentVals),
    );
    return (eq !== !!e.inv) ? e.then : e.els;
}

// One crossing per batch. Clones `count` rows of `proto`,
// stamps scope ids, evaluates + applies the plan's item-rooted
// bindings, collects binding/listener nodes, and inserts the
// whole fragment before `anchor`. Returns a flat array of
// [root, ...bindingNodes, ...listenerNodes] per row — stride is
// 1 + bindings.length + listeners.length.
export function pp_chan_mount_rows(
    planSlot, proto, anchor, items, start, count, total,
    scopeIds, scopeKey, ctxKey, parentVals,
) {
    const plan = plans[planSlot];
    const bs = plan.b, ls = plan.l;
    // Validate every node path against the prototype ONCE before
    // touching the DOM — an unresolvable path (macro emission
    // bug) returns null so the Rust side falls back to the
    // direct mount path instead of throwing mid-batch.
    for (let b = 0; b < bs.length; b++) {
        if (!walkChecked(proto, bs[b].path)) return null;
    }
    for (let l = 0; l < ls.length; l++) {
        if (!walkChecked(proto, ls[l])) return null;
    }
    const frag = document.createDocumentFragment();
    const out = [];
    for (let r = 0; r < count; r++) {
        const i = start + r;
        const item = items[i];
        const root = proto.cloneNode(true);
        const sid = scopeIds[r];
        root[scopeKey] = sid;
        root[ctxKey] = sid;
        out.push(root);
        for (let b = 0; b < bs.length; b++) {
            const B = bs[b];
            const node = walk(root, B.path);
            out.push(node);
            if (B.skip) continue;
            const v = evalExpr(B.e, item, i, total, parentVals);
            if (B.kind === 0) node.textContent = toText(v);
            else applyClass(node, v);
        }
        for (let l = 0; l < ls.length; l++) out.push(walk(root, ls[l]));
        frag.appendChild(root);
    }
    anchor.parentNode.insertBefore(frag, anchor);
    return out;
}
"#)]
extern "C" {
    fn pp_chan_register_plan(desc: &JsValue) -> u32;
    #[allow(clippy::too_many_arguments)]
    fn pp_chan_mount_rows(
        plan_slot: u32,
        proto: &Element,
        anchor: &JsValue,
        items: &JsValue,
        start: u32,
        count: u32,
        total: u32,
        scope_ids: &[f64],
        scope_key: &str,
        ctx_key: &str,
        parent_vals: &Array,
    ) -> JsValue;
}

/// A binding's contribution to the JS plan descriptor.
pub(crate) enum DescriptorExpr {
    Path {
        root: u8,
        keys: Vec<JsValue>,
    },
    TernaryEq {
        lhs: (u8, Vec<JsValue>),
        rhs: (u8, Vec<JsValue>),
        then_value: JsValue,
        else_value: JsValue,
        invert: bool,
    },
}

pub(crate) struct DescriptorBinding {
    pub node_path: &'static [u16],
    pub kind: BindingKind,
    /// Parent-dependent — the interpreter resolves the node (the
    /// list watcher needs the handle) but writes nothing.
    pub skip: bool,
    /// `None` only when `skip` — a non-skip binding always has an
    /// item-rooted expression or the plan is ineligible.
    pub expr: Option<DescriptorExpr>,
}

fn path_obj(root: u8, keys: &[JsValue]) -> JsValue {
    let o = Object::new();
    let _ = Reflect::set(&o, &"root".into(), &JsValue::from_f64(root as f64));
    let arr = Array::new();
    for k in keys {
        arr.push(k);
    }
    let _ = Reflect::set(&o, &"keys".into(), &arr);
    o.into()
}

/// Build + register the JS descriptor for a plan. One-time cost
/// per `CompiledRowPlan` (cached by the caller).
pub(crate) fn register_plan(
    bindings: &[DescriptorBinding],
    listener_paths: &[&'static [u16]],
) -> u32 {
    let desc = Object::new();
    let bs = Array::new();
    for b in bindings {
        let o = Object::new();
        let path = Array::new();
        for &idx in b.node_path {
            path.push(&JsValue::from_f64(idx as f64));
        }
        let _ = Reflect::set(&o, &"path".into(), &path);
        let kind = match b.kind {
            BindingKind::Text => 0.0,
            _ => 1.0, // Class — eligibility guarantees Text | Class
        };
        let _ = Reflect::set(&o, &"kind".into(), &JsValue::from_f64(kind));
        let _ = Reflect::set(&o, &"skip".into(), &JsValue::from_bool(b.skip));
        if let Some(expr) = &b.expr {
            let e = Object::new();
            match expr {
                DescriptorExpr::Path { root, keys } => {
                    let _ = Reflect::set(&e, &"t".into(), &JsValue::from_f64(0.0));
                    let _ = Reflect::set(&e, &"p".into(), &path_obj(*root, keys));
                }
                DescriptorExpr::TernaryEq {
                    lhs,
                    rhs,
                    then_value,
                    else_value,
                    invert,
                } => {
                    let _ = Reflect::set(&e, &"t".into(), &JsValue::from_f64(1.0));
                    let _ = Reflect::set(&e, &"l".into(), &path_obj(lhs.0, &lhs.1));
                    let _ = Reflect::set(&e, &"r".into(), &path_obj(rhs.0, &rhs.1));
                    let _ = Reflect::set(&e, &"then".into(), then_value);
                    let _ = Reflect::set(&e, &"els".into(), else_value);
                    let _ = Reflect::set(&e, &"inv".into(), &JsValue::from_bool(*invert));
                }
            }
            let _ = Reflect::set(&o, &"e".into(), &e);
        }
        bs.push(&o);
    }
    let _ = Reflect::set(&desc, &"b".into(), &bs);
    let ls = Array::new();
    for lp in listener_paths {
        let path = Array::new();
        for &idx in *lp {
            path.push(&JsValue::from_f64(idx as f64));
        }
        ls.push(&path);
    }
    let _ = Reflect::set(&desc, &"l".into(), &ls);
    pp_chan_register_plan(&desc)
}

/// One crossing: mount `count` rows of `proto` before `anchor`,
/// returning the flat handle array (stride `1 + bindings +
/// listeners`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn mount_rows(
    plan_slot: u32,
    proto: &Element,
    anchor: &JsValue,
    items: &JsValue,
    start: u32,
    count: u32,
    total: u32,
    scope_ids: &[f64],
    parent_vals: &Array,
) -> JsValue {
    pp_chan_mount_rows(
        plan_slot,
        proto,
        anchor,
        items,
        start,
        count,
        total,
        scope_ids,
        crate::mount::SCOPE_ID_KEY,
        crate::mount::CTX_PARENT_KEY,
        parent_vals,
    )
}
