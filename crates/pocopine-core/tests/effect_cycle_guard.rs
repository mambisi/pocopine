//! RFC-115 cycle guard — a divergent self-writing effect (each run
//! writes a new value into its own dependency) used to re-queue
//! forever: a silent hang with no diagnostic. The scheduler now caps
//! re-runs per uninterrupted flush cascade at 100 (the Vue/Svelte
//! convention), reports the runaway effect loudly, and suppresses it
//! until the next external trigger.
//!
//! Driven deterministically: auto-flush off, each `flush_sync` call
//! runs one flush pass; effects re-triggered during a pass land in
//! the next one, so a cascade is a run of consecutive non-empty
//! passes.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Array;
use pocopine_core::reactive::{effect, flush_sync, set_auto_flush, trigger};
use pocopine_core::signal::signal;
use pocopine_core::{ComponentState, Scope, watch_scope_field_now};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn divergent_effect_is_capped_and_recovers_after_the_cascade() {
    set_auto_flush(false);
    let (s, set) = signal(0u32);
    let runs = Rc::new(Cell::new(0u32));

    let r = runs.clone();
    let set_inner = set.clone();
    effect(move || {
        let v = s.get();
        r.set(r.get() + 1);
        // Divergent self-write: a new value every run, so the
        // equality gate in `watch()`-style consumers would never
        // save us — this is the raw runaway shape.
        set_inner.set(v + 1);
    });
    // The install run happens synchronously and queues the first
    // flush-driven re-run.
    let after_install = runs.get();
    assert_eq!(after_install, 1);

    // Drive far past the cap. Without the guard this loop never
    // settles (every pass re-queues the effect); with it, the effect
    // runs exactly 100 more times and is then suppressed, which ends
    // the cascade.
    for _ in 0..300 {
        flush_sync();
    }
    assert_eq!(
        runs.get(),
        after_install + 100,
        "divergent effect must be capped at 100 re-runs per cascade"
    );

    // The cascade ended, so the counter reset: an external write is a
    // fresh cascade and the effect fires again (and is capped again).
    set.set(9_999);
    for _ in 0..300 {
        flush_sync();
    }
    assert_eq!(
        runs.get(),
        after_install + 200,
        "suppression must lift once the cascade ends"
    );

    set_auto_flush(true);
}

struct CounterState {
    n: u32,
}

impl ComponentState for CounterState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "n" => JsValue::from_f64(self.n as f64),
            _ => JsValue::UNDEFINED,
        }
    }

    fn set(&mut self, key: &str, value: JsValue) {
        if key == "n" {
            self.n = value.as_f64().unwrap_or(0.0) as u32;
        }
    }

    fn keys(&self) -> &'static [&'static str] {
        &["n"]
    }

    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

/// The `#[watch(field)]`-shaped path: a scope-field watch whose
/// callback writes the watched field back with a new value — the
/// exact runaway RFC-115 describes. Also exercises the field label
/// stamped by `watch_scope_field_now` for the guard's report.
#[wasm_bindgen_test]
fn divergent_scope_field_watch_is_capped() {
    set_auto_flush(false);
    let state = Rc::new(RefCell::new(CounterState { n: 0 }));
    let scope = Scope::new(state.clone());
    let sid = scope.id;

    let runs = Rc::new(Cell::new(0u32));
    let r = runs.clone();
    let state_w = state.clone();
    watch_scope_field_now::<u32, _>(sid, "n", move |_next, _prev| {
        r.set(r.get() + 1);
        // Divergent self-write, the way a handler would do it:
        // mutate state, then notify the field signal.
        state_w.borrow_mut().n += 1;
        trigger(sid, "n");
    });
    // Install runs the callback once (prev = None) and queues the
    // first flush-driven re-run.
    assert_eq!(runs.get(), 1);

    for _ in 0..300 {
        flush_sync();
    }
    assert_eq!(
        runs.get(),
        1 + 100,
        "a divergent #[watch]-style handler must be capped per cascade"
    );

    set_auto_flush(true);
}

#[wasm_bindgen_test]
fn external_retriggers_are_not_mistaken_for_a_cycle() {
    set_auto_flush(false);
    let (s, set) = signal(0u32);
    let runs = Rc::new(Cell::new(0u32));

    let r = runs.clone();
    effect(move || {
        let _ = s.get();
        r.set(r.get() + 1);
    });
    assert_eq!(runs.get(), 1);

    // 150 external triggers, each its own one-pass cascade — more
    // than the cap in total, but the counter resets at every cascade
    // end, so nothing is suppressed.
    for i in 1..=150u32 {
        set.set(i);
        flush_sync();
    }
    assert_eq!(
        runs.get(),
        1 + 150,
        "well-behaved effects must never hit the per-cascade cap"
    );

    set_auto_flush(true);
}
