//! RFC-115 — `watch_scope_fields`: one payload-less subscription
//! across several named fields.
//!
//! Semantics pinned here:
//! - the initial seed runs once, deferred one tick (the install
//!   typically happens behind `on_ready`'s live borrow);
//! - same-flush triggers coalesce into one run (RFC-098 H4 queue
//!   dedup);
//! - a handler's own writes never re-fire it (echo suppression) —
//!   the payload-less form has no value gate, so this is what makes
//!   recompute handlers converge by construction;
//! - typo'd and `#[computed]` keys are rejected loudly at install
//!   and never tracked (a tracked key with no fingerprint arm would
//!   be conservatively re-triggered by every dirty sweep);
//! - runs whose field probes prove "unchanged" skip the callback.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::{Array, Promise};
use pocopine_core::reactive::{flush_sync, set_auto_flush, trigger};
use pocopine_core::watch::watch_scope_fields;
use pocopine_core::{ComponentState, Scope};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

/// Let queued microtasks (the seed's spawn_local hop) settle.
async fn settle() {
    for _ in 0..4 {
        let _ = JsFuture::from(Promise::resolve(&JsValue::NULL)).await;
    }
}

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
        &["a", "b", "c", "derived"]
    }

    fn is_computed_field(&self, key: &str) -> bool {
        key == "derived"
    }

    // Fingerprint arms for `a`/`b`/`c` — the provably-unchanged
    // probe gate needs `Some` fingerprints to skip.
    fn field_fingerprint(&self, key: &str) -> Option<u64> {
        match key {
            "a" => Some(self.a as u64),
            "b" => Some(self.b as u64),
            "c" => Some(self.c as u64),
            _ => None,
        }
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
async fn seed_defers_one_tick_and_same_flush_triggers_coalesce() {
    set_auto_flush(false);
    let (state, scope) = range_scope();
    let sid = scope.id;

    let runs = Rc::new(Cell::new(0u32));
    let r = runs.clone();
    watch_scope_fields(sid, &["a", "b", "c"], "a, b, c", move || {
        r.set(r.get() + 1);
    });
    // Install must NOT run the callback synchronously — the real
    // install site is behind on_ready's live borrow.
    assert_eq!(runs.get(), 0);
    settle().await;
    assert_eq!(runs.get(), 1, "the seed runs once, one tick later");

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

    set_auto_flush(true);
}

#[wasm_bindgen_test]
async fn self_writes_do_not_refire_the_handler() {
    set_auto_flush(false);
    let (state, scope) = range_scope();
    let sid = scope.id;

    let runs = Rc::new(Cell::new(0u32));
    let r = runs.clone();
    let state_w = state.clone();
    watch_scope_fields(sid, &["a", "b"], "a, b", move || {
        r.set(r.get() + 1);
        // Divergent self-write: a new value into a listed field on
        // every run. Echo suppression must make this converge —
        // the handler reacts to external changes only.
        let next = state_w.borrow().a + 1;
        state_w.borrow_mut().a = next;
        trigger(sid, "a");
    });
    settle().await;
    assert_eq!(runs.get(), 1, "seed runs once");

    // The seed's own write must not have queued a re-run.
    for _ in 0..50 {
        flush_sync();
    }
    assert_eq!(runs.get(), 1, "the seed's self-write must not echo");

    // An external change fires exactly one run, whose self-write is
    // again suppressed.
    state.borrow_mut().b = 7;
    trigger(sid, "b");
    for _ in 0..50 {
        flush_sync();
    }
    assert_eq!(runs.get(), 2, "external change fires once, no echo");

    set_auto_flush(true);
}

#[wasm_bindgen_test]
async fn unknown_and_computed_fields_are_rejected_at_install() {
    set_auto_flush(false);
    let (state, scope) = range_scope();
    let sid = scope.id;

    let runs = Rc::new(Cell::new(0u32));
    let r = runs.clone();
    // `nope` is a typo, `derived` is a computed key — both are
    // reported on the console and never tracked; `a` still works.
    watch_scope_fields(
        sid,
        &["a", "nope", "derived"],
        "a, nope, derived",
        move || {
            r.set(r.get() + 1);
        },
    );
    settle().await;
    assert_eq!(runs.get(), 1);

    trigger(sid, "nope");
    trigger(sid, "derived");
    flush_sync();
    assert_eq!(runs.get(), 1, "rejected keys must not be subscribed");

    state.borrow_mut().a = 5;
    trigger(sid, "a");
    flush_sync();
    assert_eq!(runs.get(), 2, "valid keys keep working");

    set_auto_flush(true);
}

#[wasm_bindgen_test]
async fn provably_unchanged_probes_skip_the_callback() {
    set_auto_flush(false);
    let (state, scope) = range_scope();
    let sid = scope.id;

    let runs = Rc::new(Cell::new(0u32));
    let r = runs.clone();
    watch_scope_fields(sid, &["a", "b"], "a, b", move || {
        r.set(r.get() + 1);
    });
    settle().await;
    assert_eq!(runs.get(), 1);

    // A trigger without an actual change: every listed fingerprint
    // is Some and unchanged, so the callback is skipped.
    trigger(sid, "a");
    flush_sync();
    assert_eq!(runs.get(), 1, "no-change trigger must be skipped");

    // A real change fires.
    state.borrow_mut().a = 42;
    trigger(sid, "a");
    flush_sync();
    assert_eq!(runs.get(), 2);

    set_auto_flush(true);
}
