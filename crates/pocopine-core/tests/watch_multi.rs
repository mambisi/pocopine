//! RFC-115 — `watch_scope_fields`: one payload-less subscription
//! across several named fields. Coalescing rides the scheduler's
//! queue dedup (RFC-098 H4): N same-flush triggers queue the effect
//! once, so the callback runs once per flush pass, and the
//! flush-cascade cycle guard bounds divergence like any other
//! queued effect.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Array;
use pocopine_core::reactive::{flush_sync, set_auto_flush, trigger};
use pocopine_core::watch::watch_scope_fields;
use pocopine_core::{ComponentState, Scope};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

struct RangeState {
    a: u32,
    b: u32,
    c: u32,
}

impl ComponentState for RangeState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "a" => JsValue::from_f64(self.a as f64),
            "b" => JsValue::from_f64(self.b as f64),
            "c" => JsValue::from_f64(self.c as f64),
            _ => JsValue::UNDEFINED,
        }
    }

    fn set(&mut self, key: &str, value: JsValue) {
        let v = value.as_f64().unwrap_or(0.0) as u32;
        match key {
            "a" => self.a = v,
            "b" => self.b = v,
            "c" => self.c = v,
            _ => {}
        }
    }

    fn keys(&self) -> &'static [&'static str] {
        &["a", "b", "c"]
    }

    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

fn range_scope() -> (Rc<RefCell<RangeState>>, Scope) {
    let state = Rc::new(RefCell::new(RangeState { a: 0, b: 0, c: 0 }));
    let scope = Scope::new(state.clone());
    (state, scope)
}

#[wasm_bindgen_test]
fn same_flush_triggers_coalesce_into_one_run() {
    set_auto_flush(false);
    let (state, scope) = range_scope();
    let sid = scope.id;

    let runs = Rc::new(Cell::new(0u32));
    let r = runs.clone();
    watch_scope_fields(sid, &["a", "b", "c"], "a, b, c", move || {
        r.set(r.get() + 1);
    });
    // Install run = the coalesced initial seed.
    assert_eq!(runs.get(), 1);

    // Three fields written before the flush — one coalesced run.
    state.borrow_mut().a = 1;
    trigger(sid, "a");
    state.borrow_mut().b = 2;
    trigger(sid, "b");
    state.borrow_mut().c = 3;
    trigger(sid, "c");
    flush_sync();
    assert_eq!(runs.get(), 2, "same-flush triggers must coalesce");

    // Separate flushes fire separately.
    state.borrow_mut().a = 10;
    trigger(sid, "a");
    flush_sync();
    state.borrow_mut().b = 20;
    trigger(sid, "b");
    flush_sync();
    assert_eq!(runs.get(), 4);

    // Unlisted fields don't fire it.
    trigger(sid, "zzz");
    flush_sync();
    assert_eq!(runs.get(), 4);

    set_auto_flush(true);
}

#[wasm_bindgen_test]
fn divergent_multi_watch_is_bounded_by_the_cycle_guard() {
    set_auto_flush(false);
    let (state, scope) = range_scope();
    let sid = scope.id;

    let runs = Rc::new(Cell::new(0u32));
    let r = runs.clone();
    let state_w = state.clone();
    watch_scope_fields(sid, &["a", "b"], "a, b", move || {
        r.set(r.get() + 1);
        // Divergent: every run writes a listed field with a new value.
        state_w.borrow_mut().a += 1;
        trigger(sid, "a");
    });
    assert_eq!(runs.get(), 1);

    for _ in 0..300 {
        flush_sync();
    }
    assert_eq!(
        runs.get(),
        1 + 100,
        "a divergent multi-field watch must be capped per cascade"
    );

    set_auto_flush(true);
}
