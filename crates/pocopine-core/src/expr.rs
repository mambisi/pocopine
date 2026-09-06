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
//! * [`evaluate`] / [`evaluate_with`] — JsValue-returning
//!   evaluator; roots resolve through the scoped access
//!   ([`ScopeAccess`]) with the proxy as residual fallback.

use std::{cell::RefCell, collections::HashMap};

use js_sys::Reflect;
use wasm_bindgen::JsValue;

pub use pocopine_expr::{BinOp, Expr, Literal, ParseError, Span, Spanned, parse};

#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum StaticLiteral {
    Null,
    Bool(bool),
    Number(f64),
    String(&'static str),
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum StaticBinOp {
    Plus,
    And,
    Or,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum StaticExpr {
    Literal(StaticLiteral),
    Path(&'static [&'static str]),
    Not(&'static StaticExpr),
    Ternary(
        &'static StaticExpr,
        &'static StaticExpr,
        &'static StaticExpr,
    ),
    #[cfg(feature = "locale")]
    Translation(&'static crate::locale::template::TranslationPlan),
    BinOp {
        op: StaticBinOp,
        lhs: &'static StaticExpr,
        rhs: &'static StaticExpr,
    },
}

impl StaticExpr {
    #[doc(hidden)]
    pub fn evaluate(&'static self, scope: &JsValue) -> JsValue {
        self.evaluate_with(scope, None)
    }

    /// [`Self::evaluate`] with an optional [`RootAccess`] for
    /// proxy-free root-path resolution (RFC-095 W1).
    #[doc(hidden)]
    pub fn evaluate_with(&'static self, scope: &JsValue, root: Option<&RootAccess>) -> JsValue {
        match self {
            StaticExpr::Literal(lit) => static_lit_to_js(lit),
            StaticExpr::Path(segments) => resolve_static_segments_with(scope, segments, root),
            StaticExpr::Not(inner) => {
                JsValue::from_bool(inner.evaluate_with(scope, root).is_falsy())
            }
            StaticExpr::Ternary(condition, yes, no) => {
                if condition.evaluate_with(scope, root).is_falsy() {
                    no.evaluate_with(scope, root)
                } else {
                    yes.evaluate_with(scope, root)
                }
            }
            #[cfg(feature = "locale")]
            StaticExpr::Translation(plan) => crate::locale::template::value(plan, scope, root),
            StaticExpr::BinOp { op, lhs, rhs } => match op {
                StaticBinOp::Plus => {
                    let l = lhs.evaluate_with(scope, root);
                    let r = rhs.evaluate_with(scope, root);
                    if l.as_string().is_some() || r.as_string().is_some() {
                        JsValue::from_str(&format!("{}{}", js_to_string(&l), js_to_string(&r)))
                    } else if let (Some(a), Some(b)) = (l.as_f64(), r.as_f64()) {
                        JsValue::from_f64(a + b)
                    } else {
                        JsValue::from_str("")
                    }
                }
                StaticBinOp::And => {
                    let l = lhs.evaluate_with(scope, root);
                    if l.is_falsy() {
                        l
                    } else {
                        rhs.evaluate_with(scope, root)
                    }
                }
                StaticBinOp::Or => {
                    let l = lhs.evaluate_with(scope, root);
                    if !l.is_falsy() {
                        l
                    } else {
                        rhs.evaluate_with(scope, root)
                    }
                }
                StaticBinOp::Eq | StaticBinOp::Ne => {
                    let l = lhs.evaluate_with(scope, root);
                    let r = rhs.evaluate_with(scope, root);
                    let eq = js_strict_eq(&l, &r);
                    JsValue::from_bool(if matches!(op, StaticBinOp::Eq) {
                        eq
                    } else {
                        !eq
                    })
                }
                StaticBinOp::Lt | StaticBinOp::Le | StaticBinOp::Gt | StaticBinOp::Ge => {
                    let l = lhs.evaluate_with(scope, root);
                    let r = rhs.evaluate_with(scope, root);
                    match (l.as_f64(), r.as_f64()) {
                        (Some(a), Some(b)) => JsValue::from_bool(match op {
                            StaticBinOp::Lt => a < b,
                            StaticBinOp::Le => a <= b,
                            StaticBinOp::Gt => a > b,
                            StaticBinOp::Ge => a >= b,
                            _ => unreachable!(),
                        }),
                        _ => JsValue::from_bool(false),
                    }
                }
            },
        }
    }
}

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

/// RFC-095 W1 / RFC-096 S1 — pluggable root-segment access.
///
/// `read` returns `Some(value)` to resolve an expression path's
/// FIRST segment Rust-side (track + cache + `ComponentState::get`,
/// no proxy trap), or `None` to fall back to `Reflect::get`
/// against the scope proxy (magics, `$`-names). Nested segments
/// always walk the resolved plain value with `Reflect`.
///
/// `write` (RFC-096 S1 — the write mirror) commits a root-field
/// assignment through `scope::write_field_tracked` — the set
/// trap's body as a plain function — returning `false` for keys
/// the access doesn't own (`$`-names), in which case the caller
/// falls back to `Reflect::set` on the proxy (the trap).
pub trait ScopeAccess {
    fn read(&self, key: &str) -> Option<JsValue>;
    fn write(&self, key: &str, value: &JsValue) -> bool;
}

pub type RootAccess = std::rc::Rc<dyn ScopeAccess>;

/// Evaluate the AST against a scope proxy. The evaluator tracks deps
/// as a side effect of `Reflect::get` calls — short-circuited
/// branches don't run and therefore don't subscribe.
pub fn evaluate(expr: &Spanned<Expr>, scope: &JsValue) -> JsValue {
    evaluate_with(expr, scope, None)
}

/// [`evaluate`] with an optional [`RootAccess`] for proxy-free
/// root-path resolution (RFC-095 W1). `evaluate(e, s)` ≡
/// `evaluate_with(e, s, None)`.
pub fn evaluate_with(expr: &Spanned<Expr>, scope: &JsValue, root: Option<&RootAccess>) -> JsValue {
    match &expr.value {
        Expr::Literal(l) => lit_to_js(l),
        Expr::Path(segs) => resolve_segments_with(scope, segs, root),
        Expr::Not(inner) => JsValue::from_bool(evaluate_with(inner, scope, root).is_falsy()),
        Expr::BinOp(op, lhs, rhs) => match op {
            BinOp::And => {
                let l = evaluate_with(lhs, scope, root);
                if l.is_falsy() {
                    l
                } else {
                    evaluate_with(rhs, scope, root)
                }
            }
            BinOp::Or => {
                let l = evaluate_with(lhs, scope, root);
                if !l.is_falsy() {
                    l
                } else {
                    evaluate_with(rhs, scope, root)
                }
            }
            BinOp::Eq | BinOp::Ne => {
                let l = evaluate_with(lhs, scope, root);
                let r = evaluate_with(rhs, scope, root);
                let eq = js_strict_eq(&l, &r);
                let out = if matches!(op, BinOp::Eq) { eq } else { !eq };
                JsValue::from_bool(out)
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let l = evaluate_with(lhs, scope, root);
                let r = evaluate_with(rhs, scope, root);
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
                let l = evaluate_with(lhs, scope, root);
                let r = evaluate_with(rhs, scope, root);
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
            if !evaluate_with(cond, scope, root).is_falsy() {
                evaluate_with(then_e, scope, root)
            } else {
                evaluate_with(else_e, scope, root)
            }
        }
        Expr::Call(name, args) => {
            // Evaluate args left-to-right into a JS Array that
            // `invoke_handler` can pass through `FromHandlerArg`.
            let arr = js_sys::Array::new();
            for a in args {
                arr.push(&evaluate_with(a, scope, root));
            }
            // RFC-095 — no magic resolves to a callable anymore
            // (`$dispatch` was the only one; removed in favor of
            // the Rust-side `emit*` family). Guard the `$`-call
            // shape with a loud warning instead of routing it to
            // `invoke_handler`, which would silently return
            // `undefined` for a name no user handler can have.
            if name.starts_with('$') {
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "pocopine: `{name}(...)` — callable magics were removed; \
                     dispatch events from a Rust handler via `emit` instead"
                )));
                return JsValue::UNDEFINED;
            }
            match scope_id_for(scope) {
                Some(id) => crate::scope::invoke_handler(id, name, &arr),
                None => JsValue::UNDEFINED,
            }
        }
        Expr::Assign(path, rhs) => {
            // RFC-096 S1 — assignments route through the scoped
            // writer when an access owns the root key; the proxy
            // set trap remains the fallback (and itself delegates
            // to the same `write_field_tracked`, so the two paths
            // cannot diverge).
            let v = evaluate_with(rhs, scope, root);
            write_assign_path_with(scope, path, &v, root);
            v
        }
        Expr::Seq(stmts) => {
            let mut last = JsValue::UNDEFINED;
            for s in stmts {
                last = evaluate_with(s, scope, root);
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

/// Apply an assignment to a scope — [`crate::path::write_segments_with`]
/// over the parsed segments, the same core `pp-model` writes ride,
/// so `@click="a.b = x"` and `pp-model="a.b"` cannot diverge.
/// Single-segment paths go through the scoped writer; dotted paths
/// mutate the read snapshot and the core surfaces the write by
/// writing the field back (RFC-024 §7, deepened).
fn write_assign_path_with(
    proxy: &JsValue,
    segments: &[String],
    value: &JsValue,
    root: Option<&RootAccess>,
) {
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let _ = crate::path::write_segments_with(proxy, root, &segments, value);
}

fn lit_to_js(l: &Literal) -> JsValue {
    match l {
        Literal::Null => JsValue::NULL,
        Literal::Bool(b) => JsValue::from_bool(*b),
        Literal::Number(n) => JsValue::from_f64(*n),
        Literal::String(s) => JsValue::from_str(s),
    }
}

fn static_lit_to_js(l: &StaticLiteral) -> JsValue {
    match l {
        StaticLiteral::Null => JsValue::NULL,
        StaticLiteral::Bool(b) => JsValue::from_bool(*b),
        StaticLiteral::Number(n) => JsValue::from_f64(*n),
        StaticLiteral::String(s) => JsValue::from_str(s),
    }
}

fn resolve_segments_with(
    scope: &JsValue,
    segments: &[String],
    root: Option<&RootAccess>,
) -> JsValue {
    let Some((first, rest)) = segments.split_first() else {
        return JsValue::UNDEFINED;
    };
    // RFC-096 S2 — `$store.<name>.field…` / `$route.field…` ride
    // the backing scope's reader instead of proxy objects, when a
    // field segment exists past the magic root.
    if first.starts_with('$')
        && let Some((access, consumed)) =
            crate::scope::magic_scope_access(first, segments.get(1).map(|s| s.as_str()))
        && let Some(field) = segments.get(consumed)
    {
        let mut cur = access.read(field).unwrap_or(JsValue::UNDEFINED);
        for seg in &segments[consumed + 1..] {
            cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
        }
        return cur;
    }
    // RFC-095 W1 — the root segment is the only one that touches
    // scope state; resolve it Rust-side when a reader owns it.
    // The resolved value is a plain JsValue (cached serde output),
    // so the remaining segments walk it with trap-free `Reflect`.
    let mut cur = match root.and_then(|a| a.read(first)) {
        Some(v) => v,
        None => Reflect::get(scope, &JsValue::from_str(first)).unwrap_or(JsValue::UNDEFINED),
    };
    for seg in rest {
        cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
    }
    cur
}

fn resolve_static_segments_with(
    scope: &JsValue,
    segments: &[&'static str],
    root: Option<&RootAccess>,
) -> JsValue {
    let Some((first, rest)) = segments.split_first() else {
        return JsValue::UNDEFINED;
    };
    if first.starts_with('$')
        && let Some((access, consumed)) =
            crate::scope::magic_scope_access(first, segments.get(1).copied())
        && let Some(field) = segments.get(consumed)
    {
        let mut cur = access.read(field).unwrap_or(JsValue::UNDEFINED);
        for seg in &segments[consumed + 1..] {
            cur = Reflect::get(&cur, &JsValue::from_str(seg)).unwrap_or(JsValue::UNDEFINED);
        }
        return cur;
    }
    let mut cur = match root.and_then(|a| a.read(first)) {
        Some(v) => v,
        None => Reflect::get(scope, &JsValue::from_str(first)).unwrap_or(JsValue::UNDEFINED),
    };
    for seg in rest {
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
        // JS String() semantics via the shared helper — also
        // strips the `.0` for integers so `$id + '-' + 3` reads
        // `pp-1-3`, not `pp-1-3.0`.
        return crate::text::js_number_string(n);
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
