//! Dotted-path resolution for directive values.
//!
//! Turns `"$store.preferences.theme"` into a chain of `Reflect::get`
//! calls. Each intermediate read fires the corresponding proxy's `get`
//! trap — which calls [`crate::reactive::track`] — so dep tracking
//! walks the whole path naturally. Terminal reads of plain fields do
//! the same.

use js_sys::Reflect;
use wasm_bindgen::JsValue;

/// Walk `path` (dot-separated) starting from `root`, `Reflect::get`-ing
/// each segment. Missing / malformed segments resolve to
/// `JsValue::UNDEFINED`.
pub fn resolve_path(root: &JsValue, path: &str) -> JsValue {
    path.split('.').fold(root.clone(), |acc, segment| {
        if segment.is_empty() {
            return acc;
        }
        Reflect::get(&acc, &JsValue::from_str(segment)).unwrap_or(JsValue::UNDEFINED)
    })
}

/// Evaluate `src` as a truthiness test. Historically handled only a
/// leading `!` + dotted path; now a thin wrapper over the RFC-012
/// expression grammar. On parse error, returns `false` — callers
/// doing their own error reporting (pp-show / pp-if) handle parse
/// failures at setup, not here.
pub fn resolve_truthy(root: &JsValue, src: &str) -> bool {
    let Ok(ast) = crate::expr::parse(src) else {
        return false;
    };
    crate::expr::evaluate_truthy(&ast, root)
}

/// Write the final segment of `path` on the deepest reachable object.
/// Used by `pp-model` for dotted-path two-way bindings (`$store.foo.bar`).
/// Returns `true` if the write succeeded.
pub fn write_path(root: &JsValue, path: &str, value: &JsValue) -> bool {
    let mut segments: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    let Some(last) = segments.pop() else {
        return false;
    };
    let mut target = root.clone();
    for segment in segments {
        let next = Reflect::get(&target, &JsValue::from_str(segment)).unwrap_or(JsValue::UNDEFINED);
        if next.is_undefined() || next.is_null() {
            return false;
        }
        target = next;
    }
    Reflect::set(&target, &JsValue::from_str(last), value).unwrap_or(false)
}
