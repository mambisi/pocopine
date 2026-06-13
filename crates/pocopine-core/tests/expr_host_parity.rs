//! RFC-099 Phase 1b parity gate — the host `serde_json` evaluator
//! (`host_eval::eval`) agrees with the wasm `JsValue` evaluator
//! (`expr::evaluate`) on the SAME parsed AST, over a fuzz corpus of
//! random state + random expressions. This is what guarantees the SSR
//! server stamps the value the client would compute, despite the two
//! evaluators being separate implementations.
//!
//! Runs under `wasm-pack test --node crates/pocopine-core`.
//!
//! State values are bool/number/string only (not JSON `null`): a
//! `serde_json::Value::Null` serializes to JS `undefined`, and JS
//! `undefined === null` is `false` while host `Null === Null` is true
//! — a null-vs-undefined boundary that's a Phase-2 concern (how
//! `Option::None` state serializes). `null` *literals* in expressions
//! are still exercised (real `null` on both sides). Likewise the
//! generator emits no nested paths (which could surface `undefined`)
//! and only non-negative number literals (the grammar has `+` but no
//! subtraction / unary minus).

#![cfg(target_arch = "wasm32")]

use pocopine_core::{expr, host_eval};
use serde::Serialize;
use serde_json::{Map, Number, Value};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn below(&mut self, n: u32) -> u32 {
        ((self.next() >> 33) % n as u64) as u32
    }
}

fn num(f: f64) -> Value {
    Value::Number(Number::from_f64(f).expect("finite"))
}

fn rand_state(rng: &mut Rng) -> Value {
    let mut o = Map::new();
    for k in ["a", "b", "c", "d"] {
        let v = match rng.below(4) {
            0 => Value::Bool(rng.below(2) == 1),
            1 => num((rng.below(20) as f64) - 10.0), // integral -10..9
            2 => num(((rng.below(2000) as f64) / 100.0) - 10.0), // float -10..10
            _ => Value::String(["", "x", "5", "hi"][rng.below(4) as usize].to_string()),
        };
        o.insert(k.to_string(), v);
    }
    Value::Object(o)
}

fn leaf(rng: &mut Rng) -> String {
    match rng.below(7) {
        0 => "a".into(),
        1 => "b".into(),
        2 => "c".into(),
        3 => "d".into(),
        4 => format!("{}", rng.below(20)), // non-negative literal
        5 => format!("\"{}\"", ["", "x", "5", "hi"][rng.below(4) as usize]),
        _ => ["true", "false", "null"][rng.below(3) as usize].into(),
    }
}

fn expr_src(rng: &mut Rng, depth: u32) -> String {
    if depth == 0 || rng.below(3) == 0 {
        return leaf(rng);
    }
    let l = expr_src(rng, depth - 1);
    match rng.below(10) {
        0 => format!("({l} && {})", expr_src(rng, depth - 1)),
        1 => format!("({l} || {})", expr_src(rng, depth - 1)),
        2 => format!("({l} == {})", expr_src(rng, depth - 1)),
        3 => format!("({l} != {})", expr_src(rng, depth - 1)),
        4 => format!("({l} < {})", expr_src(rng, depth - 1)),
        5 => format!("({l} > {})", expr_src(rng, depth - 1)),
        6 => format!("({l} <= {})", expr_src(rng, depth - 1)),
        7 => format!("({l} + {})", expr_src(rng, depth - 1)),
        8 => format!("!{l}"),
        _ => format!(
            "({l} ? {} : {})",
            expr_src(rng, depth - 1),
            expr_src(rng, depth - 1)
        ),
    }
}

/// Compare by value, ignoring serde_json int/float representation (JS
/// has a single number type).
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

#[wasm_bindgen_test]
fn host_and_js_evaluators_agree_over_fuzz() {
    let mut rng = Rng(0xDEAD_BEEF_1234_5678);
    // Plain-object serialization so the JS evaluator's `Reflect::get`
    // hits (the default would emit an ES `Map`, which Reflect can't
    // index — mirrors how a `#[component]` struct serializes).
    let ser = serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true);
    let mut checked = 0u32;
    for _ in 0..6000 {
        let state = rand_state(&mut rng);
        let src = expr_src(&mut rng, 3);
        let Ok(ast) = expr::parse_cached(&src) else {
            continue;
        };
        let host = host_eval::eval(&ast, &state);
        let scope: JsValue = state.serialize(&ser).expect("state → JsValue");
        let js = expr::evaluate(&ast, &scope);
        let js_json: Value = serde_wasm_bindgen::from_value(js).unwrap_or(Value::Null);
        assert!(
            loose_eq(&host, &js_json),
            "DRIFT src={src}\n  state={state}\n  host={host}\n  js  ={js_json}"
        );
        checked += 1;
    }
    assert!(
        checked > 3000,
        "fuzz ran too few parseable cases: {checked}"
    );
}
