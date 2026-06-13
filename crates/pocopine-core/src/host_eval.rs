//! RFC-099 Phase 1b — host-side expression evaluator over
//! `serde_json::Value`.
//!
//! Mirrors [`crate::expr::evaluate_with`]'s JavaScript semantics for
//! the read-only subset SSR rendering needs, so the server stamps the
//! exact value the wasm client would compute. It evaluates the SAME
//! parsed AST (`pocopine_expr::Expr`) — only the value type and the
//! field-access backend differ (`serde_json::Value` + object indexing
//! here, vs `JsValue` + `Reflect::get` on the client).
//!
//! Why a parallel evaluator rather than genericizing `evaluate_with`
//! over a `ValueBackend` trait: the client evaluator is the RFC-095/096
//! reactive hot path, deeply coupled to `JsValue` / `Reflect` /
//! `ScopeAccess` / magic-scope resolution. Genericizing it risks that
//! perf-critical path; a parallel host evaluator keeps the client
//! untouched. The cost — two implementations that could drift — is
//! paid down by the cross-engine differential fuzz
//! (`tests/expr_host_parity.rs`), which evaluates the same AST through
//! both engines and asserts equal results.
//!
//! Semantics deliberately mirror the client evaluator's helpers, not
//! "real JS": `to_js_string` returns `"[object Object]"` for arrays
//! (matching `expr::js_to_string`, which doesn't special-case arrays).
//!
//! Read-only: `Call` → `Null` (rendering never invokes handlers),
//! `Assign` → the rhs value (no write). `$`-rooted magic paths
//! (`$store` / `$route`) are not resolved host-side in Phase 1b — they
//! yield `Null` until Phase 2 wires store/route state into the render
//! context. Likewise array `.length` and nested access on non-objects
//! yield `Null` here (Phase 2 stamper concern); declared component
//! state never produces a missing key, so this matches the client for
//! every in-state path.

use pocopine_expr::{BinOp, Expr, Literal, Spanned};
use serde_json::Value;

/// Evaluate `expr` against host component `state` (the serialized
/// component struct, as a JSON object).
pub fn eval(expr: &Spanned<Expr>, state: &Value) -> Value {
    match &expr.value {
        Expr::Literal(l) => lit_to_json(l),
        Expr::Path(segs) => resolve_path(state, segs),
        Expr::Not(inner) => Value::Bool(is_falsy(&eval(inner, state))),
        Expr::BinOp(op, lhs, rhs) => eval_binop(*op, lhs, rhs, state),
        Expr::Ternary(cond, then_e, else_e) => {
            if is_falsy(&eval(cond, state)) {
                eval(else_e, state)
            } else {
                eval(then_e, state)
            }
        }
        // Rendering is read-only — handlers never fire during a stamp.
        Expr::Call(_, _) => Value::Null,
        // No write host-side; an assignment's value is its rhs.
        Expr::Assign(_, rhs) => eval(rhs, state),
        Expr::Seq(stmts) => {
            let mut last = Value::Null;
            for s in stmts {
                last = eval(s, state);
            }
            last
        }
    }
}

fn eval_binop(op: BinOp, lhs: &Spanned<Expr>, rhs: &Spanned<Expr>, state: &Value) -> Value {
    match op {
        BinOp::And => {
            let l = eval(lhs, state);
            if is_falsy(&l) {
                l
            } else {
                eval(rhs, state)
            }
        }
        BinOp::Or => {
            let l = eval(lhs, state);
            if is_falsy(&l) {
                eval(rhs, state)
            } else {
                l
            }
        }
        BinOp::Eq | BinOp::Ne => {
            let eq = strict_eq(&eval(lhs, state), &eval(rhs, state));
            Value::Bool(if matches!(op, BinOp::Eq) { eq } else { !eq })
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            match (as_f64(&eval(lhs, state)), as_f64(&eval(rhs, state))) {
                (Some(a), Some(b)) => Value::Bool(match op {
                    BinOp::Lt => a < b,
                    BinOp::Le => a <= b,
                    BinOp::Gt => a > b,
                    BinOp::Ge => a >= b,
                    _ => unreachable!(),
                }),
                _ => Value::Bool(false),
            }
        }
        BinOp::Plus => {
            let l = eval(lhs, state);
            let r = eval(rhs, state);
            if is_string(&l) || is_string(&r) {
                Value::String(format!("{}{}", to_js_string(&l), to_js_string(&r)))
            } else if let (Some(a), Some(b)) = (as_f64(&l), as_f64(&r)) {
                number(a + b)
            } else {
                Value::String(String::new())
            }
        }
    }
}

fn lit_to_json(l: &Literal) -> Value {
    match l {
        Literal::Null => Value::Null,
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Number(n) => number(*n),
        Literal::String(s) => Value::String(s.clone()),
    }
}

/// Resolve a dotted path against `state`. First segment indexes the
/// state object; nested segments index successive objects. A missing
/// key, a non-object intermediate, or a `$`-rooted magic path yields
/// `Null` (see module docs — declared state never misses an in-state
/// key, so this matches the client there).
fn resolve_path(state: &Value, segments: &[String]) -> Value {
    let Some((first, rest)) = segments.split_first() else {
        return Value::Null;
    };
    // No special-case for `$`-rooted paths: the SSR for-row stamper
    // (RFC-099 Phase 3) injects loop-locals (`$index` / `$first` /
    // `$last`) into the row state, so they resolve here like any other
    // key. True magic roots (`$store` / `$route`) are absent from state
    // and fall through to `Null` — the same result the old explicit
    // guard produced, so cross-engine parity is unaffected.
    let mut cur = state.get(first).cloned().unwrap_or(Value::Null);
    for seg in rest {
        cur = cur
            .as_object()
            .and_then(|o| o.get(seg).cloned())
            .unwrap_or(Value::Null);
    }
    cur
}

/// JS truthiness — mirrors `wasm_bindgen::JsValue::is_falsy`. Falsy:
/// `null`, `false`, `0`/`-0`, `""`. Everything else (incl. `[]` and
/// `{}`) is truthy.
fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f == 0.0),
        Value::String(s) => s.is_empty(),
        Value::Array(_) | Value::Object(_) => false,
    }
}

/// `JsValue::as_f64` analogue — `Some` only for numbers (no string
/// coercion).
fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        _ => None,
    }
}

fn is_string(v: &Value) -> bool {
    matches!(v, Value::String(_))
}

/// Mirror of `expr::js_strict_eq` (`===`): matching primitive types
/// compare by value; cross-type and object/array compare unequal (JS
/// reference inequality for distinct values).
fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Number(_), Value::Number(_)) => as_f64(a) == as_f64(b),
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

/// Mirror of `expr::js_to_string` (the `BinOp::Plus` string coercion):
/// numbers via [`crate::js_number::to_js_string`], bools as
/// `"true"`/`"false"`, null as `"null"`, arrays/objects as
/// `"[object Object]"` (the client helper's catch-all).
fn to_js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n
            .as_f64()
            .map(crate::js_number::to_js_string)
            .unwrap_or_default(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Null => "null".to_string(),
        Value::Array(_) | Value::Object(_) => "[object Object]".to_string(),
    }
}

/// Build a JSON number from an `f64`; non-finite (which `serde_json`
/// can't represent) collapses to `Null`. Finite values are the only
/// ones SSR rendering produces.
fn number(f: f64) -> Value {
    serde_json::Number::from_f64(f)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::eval;
    use serde_json::{json, Value};

    fn run(src: &str, state: &Value) -> Value {
        let ast = crate::expr::parse(src).expect("parse");
        eval(&ast, state)
    }

    /// Compare by value, not by serde_json int/float representation
    /// (JS has a single number type; `Number(3)` and `Number(3.0)` are
    /// the same value). Matches the differential fuzz's comparison.
    fn loose_eq(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
            (Value::Array(xs), Value::Array(ys)) => {
                xs.len() == ys.len() && xs.iter().zip(ys).all(|(p, q)| loose_eq(p, q))
            }
            (Value::Object(xs), Value::Object(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .all(|(k, v)| ys.get(k).is_some_and(|w| loose_eq(v, w)))
            }
            _ => a == b,
        }
    }

    #[test]
    fn mirrors_client_evaluator_semantics() {
        let s = json!({ "n": 3, "z": 0, "s": "hi", "empty": "", "t": true, "f": false,
                        "nil": null, "obj": {"k": 7} });
        let check = |src: &str, want: Value| {
            let got = run(src, &s);
            assert!(loose_eq(&got, &want), "{src}: got {got}, want {want}");
        };

        // paths + nesting
        check("n", json!(3));
        check("obj.k", json!(7));
        check("missing", json!(null));

        // truthiness / not
        check("!z", json!(true)); // 0 is falsy
        check("!empty", json!(true)); // "" is falsy
        check("!obj", json!(false)); // object is truthy

        // && / || short-circuit, returning the operand (JS semantics)
        check("z && n", json!(0)); // falsy lhs returned
        check("n && s", json!("hi")); // truthy lhs → rhs
        check("empty || s", json!("hi"));

        // strict equality (no coercion): number 3 !== string "3"
        check("n == 3", json!(true));
        check("n == \"3\"", json!(false));
        check("nil == null", json!(true));
        check("s != \"no\"", json!(true));

        // comparisons (numeric only)
        check("n < 5", json!(true));
        check("s < 5", json!(false)); // string vs num → false

        // plus: string-concat (number via JS formatting) vs numeric add
        check("s + n", json!("hi3"));
        check("n + n", json!(6));
        check("\"x\" + t", json!("xtrue"));

        // ternary
        check("t ? s : n", json!("hi"));
        check("f ? s : n", json!(3));

        // read-only: call → null, assign → rhs
        check("doThing()", json!(null));
        check("n = 9", json!(9));
    }
}
