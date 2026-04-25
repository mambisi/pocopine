//! Template expression evaluator + thread-local parse cache.
//!
//! The grammar, lexer, parser, and AST live in the
//! [`pocopine_expr`] crate so the proc-macro can validate
//! expressions at `cargo check` time without pulling in
//! `wasm-bindgen` / `web-sys`. This module re-exports those
//! types verbatim (back-compat for every `crate::expr::Expr`,
//! `crate::expr::Spanned<…>` consumer in the runtime) and adds
//! the runtime-only pieces:
//!
//! * [`parse_cached`] — thread-local memo over `pocopine_expr::parse`.
//! * [`evaluate`] / [`evaluate_truthy`] — JsValue-returning
//!   evaluator that walks the AST against a scope proxy and
//!   tracks dependencies via `Reflect::get`.

use std::{cell::RefCell, collections::HashMap};

use js_sys::Reflect;
use wasm_bindgen::{JsCast, JsValue};

pub use pocopine_expr::{BinOp, Expr, Literal, ParseError, Span, Spanned, parse};

thread_local! {
    static PARSE_CACHE: RefCell<HashMap<String, Result<Spanned<Expr>, ParseError>>> =
        RefCell::new(HashMap::new());
}

/// Parse `src`, memoising the result by source string. Used by
/// every directive that re-evaluates an expression on each
/// reactivity tick — first hit pays the parse cost; the rest
/// reuse the cached AST. Both successful and failed parses are
/// cached so we don't re-run the parser on persistent error
/// strings either.
pub fn parse_cached(src: &str) -> Result<Spanned<Expr>, ParseError> {
    PARSE_CACHE.with(|cache| {
        if let Some(hit) = cache.borrow().get(src).cloned() {
            return hit;
        }
        let parsed = parse(src);
        cache.borrow_mut().insert(src.to_string(), parsed.clone());
        parsed
    })
}

// ─── evaluator ────────────────────────────────────────────────────

/// Evaluate the AST against a scope proxy. The evaluator tracks deps
/// as a side effect of `Reflect::get` calls — short-circuited
/// branches don't run and therefore don't subscribe.
pub fn evaluate(expr: &Spanned<Expr>, scope: &JsValue) -> JsValue {
    match &expr.value {
        Expr::Literal(l) => lit_to_js(l),
        Expr::Path(segs) => resolve_segments(scope, segs),
        Expr::Not(inner) => JsValue::from_bool(evaluate(inner, scope).is_falsy()),
        Expr::BinOp(op, lhs, rhs) => match op {
            BinOp::And => {
                let l = evaluate(lhs, scope);
                if l.is_falsy() {
                    l
                } else {
                    evaluate(rhs, scope)
                }
            }
            BinOp::Or => {
                let l = evaluate(lhs, scope);
                if !l.is_falsy() {
                    l
                } else {
                    evaluate(rhs, scope)
                }
            }
            BinOp::Eq | BinOp::Ne => {
                let l = evaluate(lhs, scope);
                let r = evaluate(rhs, scope);
                let eq = js_strict_eq(&l, &r);
                let out = if matches!(op, BinOp::Eq) { eq } else { !eq };
                JsValue::from_bool(out)
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let l = evaluate(lhs, scope);
                let r = evaluate(rhs, scope);
                match (l.as_f64(), r.as_f64()) {
                    (Some(a), Some(b)) => JsValue::from_bool(match op {
                        BinOp::Lt => a < b,
                        BinOp::Le => a <= b,
                        BinOp::Gt => a > b,
                        BinOp::Ge => a >= b,
                        _ => unreachable!(),
                    }),
                    _ => JsValue::from_bool(false),
                }
            }
            BinOp::Plus => {
                let l = evaluate(lhs, scope);
                let r = evaluate(rhs, scope);
                if l.as_string().is_some() || r.as_string().is_some() {
                    let ls = js_to_string(&l);
                    let rs = js_to_string(&r);
                    JsValue::from_str(&format!("{ls}{rs}"))
                } else if let (Some(a), Some(b)) = (l.as_f64(), r.as_f64()) {
                    JsValue::from_f64(a + b)
                } else {
                    JsValue::from_str("")
                }
            }
        },
        Expr::Ternary(cond, then_e, else_e) => {
            if !evaluate(cond, scope).is_falsy() {
                evaluate(then_e, scope)
            } else {
                evaluate(else_e, scope)
            }
        }
        Expr::Call(name, args) => {
            // Evaluate args left-to-right into a JS Array that
            // `invoke_handler` can pass through `FromHandlerArg`.
            let arr = js_sys::Array::new();
            for a in args {
                arr.push(&evaluate(a, scope));
            }
            // Magics in call position: `$dispatch(name, detail)` —
            // `magics::resolve` returns a JS `Function`, which we
            // `.apply()` with the evaluated args. Without this branch
            // the lookup went through `invoke_handler` and silently
            // returned `undefined` because there's no user handler
            // named `$dispatch`.
            if name.starts_with('$') {
                if let Some(id) = scope_id_for(scope) {
                    let fn_val = crate::magics::resolve(name, id);
                    if let Some(f) = fn_val.dyn_ref::<js_sys::Function>() {
                        return f
                            .apply(&JsValue::UNDEFINED, &arr)
                            .unwrap_or(JsValue::UNDEFINED);
                    }
                }
                return JsValue::UNDEFINED;
            }
            match scope_id_for(scope) {
                Some(id) => crate::scope::invoke_handler(id, name, &arr),
                None => JsValue::UNDEFINED,
            }
        }
        Expr::Assign(path, rhs) => {
            let v = evaluate(rhs, scope);
            write_assign_path(scope, path, &v);
            v
        }
        Expr::Seq(stmts) => {
            let mut last = JsValue::UNDEFINED;
            for s in stmts {
                last = evaluate(s, scope);
            }
            last
        }
    }
}

/// Route `Expr::Call` to `invoke_handler` via the thread-local
/// scope id set by directives around their `evaluate` call. We
/// avoid threading scope_id through every evaluator site by
/// reading the already-ambient `CURRENT_SCOPE_ID` — directives
/// like `pp-on` that actually support call syntax wrap evaluation
/// in `with_current_scope_id`.
fn scope_id_for(_proxy: &JsValue) -> Option<crate::reactive::ScopeId> {
    crate::scope::current_scope_id()
}

/// Apply an assignment to a scope. Single-segment paths go through
/// the scope's `set` trap (full reactivity). Multi-segment paths
/// read the penultimate object (subscribing reads along the way)
/// and set the final segment in place — reactivity fires on the
/// outer object's `set` only if the author surfaces the write by
/// rewriting the field, per RFC-024 §7.
fn write_assign_path(proxy: &JsValue, segments: &[String], value: &JsValue) {
    if segments.is_empty() {
        return;
    }
    if segments.len() == 1 {
        let _ = Reflect::set(proxy, &JsValue::from_str(&segments[0]), value);
        return;
    }
    let mut cur = proxy.clone();
    for seg in &segments[..segments.len() - 1] {
        cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
        if !cur.is_object() {
            return;
        }
    }
    let last = &segments[segments.len() - 1];
    let _ = Reflect::set(&cur, &JsValue::from_str(last), value);
}

fn lit_to_js(l: &Literal) -> JsValue {
    match l {
        Literal::Null => JsValue::NULL,
        Literal::Bool(b) => JsValue::from_bool(*b),
        Literal::Number(n) => JsValue::from_f64(*n),
        Literal::String(s) => JsValue::from_str(s),
    }
}

fn resolve_segments(root: &JsValue, segments: &[String]) -> JsValue {
    let mut cur = root.clone();
    for seg in segments {
        cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

/// Loose string coercion used by `BinOp::Plus` when either operand
/// is a string. Handles the common primitives; for objects, returns
/// `"[object Object]"` (the default JS `toString()` shape).
fn js_to_string(v: &JsValue) -> String {
    if let Some(s) = v.as_string() {
        return s;
    }
    if let Some(n) = v.as_f64() {
        // Strip the `.0` for integers so `$id + '-' + 3` reads
        // `pp-1-3`, not `pp-1-3.0`.
        if n.fract() == 0.0 && n.is_finite() {
            return format!("{}", n as i64);
        }
        return n.to_string();
    }
    if let Some(b) = v.as_bool() {
        return if b { "true".into() } else { "false".into() };
    }
    if v.is_null() {
        return "null".into();
    }
    if v.is_undefined() {
        return "undefined".into();
    }
    "[object Object]".into()
}

/// Strict equality — primitive types compare by value, everything
/// else compares by JS reference semantics. No type coercion.
fn js_strict_eq(a: &JsValue, b: &JsValue) -> bool {
    if let (Some(as_), Some(bs_)) = (a.as_string(), b.as_string()) {
        return as_ == bs_;
    }
    if let (Some(an), Some(bn)) = (a.as_f64(), b.as_f64()) {
        return an == bn;
    }
    if let (Some(ab), Some(bb)) = (a.as_bool(), b.as_bool()) {
        return ab == bb;
    }
    // Null / undefined quirks.
    if a.is_null() && b.is_null() {
        return true;
    }
    if a.is_undefined() && b.is_undefined() {
        return true;
    }
    // Fall back to referential equality via js_sys::Object::is.
    js_sys::Object::is(a, b)
}

/// Top-level truthiness evaluation — convenience for
/// `pp-show` / `pp-if`. Short form of `parse` + `evaluate` + `!is_falsy`.
pub fn evaluate_truthy(expr: &Spanned<Expr>, scope: &JsValue) -> bool {
    !evaluate(expr, scope).is_falsy()
}
