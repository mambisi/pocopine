//! Unit tests for the reactive core: effects, cleanups, batching,
//! signals, computed, watch. Runs under `wasm-pack test --node`.

#![cfg(target_arch = "wasm32")]

use std::cell::Cell;
use std::rc::Rc;

use pocopine_core::{
    batch, computed, effect, flush_sync, on_cleanup, release, rw_signal, set_auto_flush,
    signal, watch,
};
use wasm_bindgen_test::wasm_bindgen_test;

fn setup() {
    // spawn_local's microtask host isn't reliable under
    // `wasm-pack test --node`; drive flushes manually.
    set_auto_flush(false);
}

#[wasm_bindgen_test]
fn signal_get_set_basic() {
    setup();
    let (s, setter) = signal(0_i32);
    assert_eq!(s.get(), 0);
    setter.set(7);
    assert_eq!(s.get(), 7);
}

#[wasm_bindgen_test]
fn effect_reruns_on_signal_change() {
    setup();
    let (s, setter) = signal(0_i32);
    let seen = Rc::new(Cell::new(-1));
    let seen_w = seen.clone();
    let s_clone = s.clone();
    effect(move || {
        seen_w.set(s_clone.get());
    });
    assert_eq!(seen.get(), 0);

    setter.set(3);
    flush_sync();
    assert_eq!(seen.get(), 3);

    setter.set(10);
    flush_sync();
    assert_eq!(seen.get(), 10);
}

#[wasm_bindgen_test]
fn dep_cleared_on_rerun_conditional_read() {
    setup();
    // Effect reads `a` always, `b` only when `flag` is true. After `flag`
    // flips to false, subsequent `b` changes must not rerun the effect.
    let (a, set_a) = signal(1_i32);
    let (b, set_b) = signal(100_i32);
    let (flag, set_flag) = signal(true);

    let runs = Rc::new(Cell::new(0));
    let runs_w = runs.clone();

    let a_c = a.clone();
    let b_c = b.clone();
    let flag_c = flag.clone();
    effect(move || {
        let _ = a_c.get();
        if flag_c.get() {
            let _ = b_c.get();
        }
        runs_w.set(runs_w.get() + 1);
    });
    assert_eq!(runs.get(), 1);

    // flip flag -> rerun drops dep on b
    set_flag.set(false);
    flush_sync();
    assert_eq!(runs.get(), 2);

    // b changes now — effect should NOT rerun
    set_b.set(200);
    flush_sync();
    assert_eq!(runs.get(), 2, "stale dep on b was not cleared");

    // a still tracked
    set_a.set(2);
    flush_sync();
    assert_eq!(runs.get(), 3);
}

#[wasm_bindgen_test]
fn on_cleanup_fires_before_rerun() {
    setup();
    let (s, setter) = signal(0_i32);
    let cleanups = Rc::new(Cell::new(0));
    let cleanups_w = cleanups.clone();
    let s_c = s.clone();
    effect(move || {
        let _ = s_c.get();
        let cw = cleanups_w.clone();
        on_cleanup(move || cw.set(cw.get() + 1));
    });
    // First run — one cleanup registered, not yet fired.
    assert_eq!(cleanups.get(), 0);

    setter.set(1);
    flush_sync();
    // Cleanup from run 1 must have fired before run 2 started.
    assert_eq!(cleanups.get(), 1);

    setter.set(2);
    flush_sync();
    assert_eq!(cleanups.get(), 2);
}

#[wasm_bindgen_test]
fn on_cleanup_fires_on_release() {
    setup();
    let (s, _setter) = signal(0_i32);
    let fired = Rc::new(Cell::new(false));
    let fired_w = fired.clone();
    let s_c = s.clone();
    let id = effect(move || {
        let _ = s_c.get();
        let fw = fired_w.clone();
        on_cleanup(move || fw.set(true));
    });
    assert!(!fired.get());
    release(id);
    assert!(fired.get(), "release did not invoke the registered cleanup");
}

#[wasm_bindgen_test]
fn batch_coalesces_multiple_writes_into_one_rerun() {
    setup();
    let (a, set_a) = signal(0_i32);
    let (b, set_b) = signal(0_i32);
    let runs = Rc::new(Cell::new(0));
    let runs_w = runs.clone();
    let a_c = a.clone();
    let b_c = b.clone();
    effect(move || {
        let _ = a_c.get();
        let _ = b_c.get();
        runs_w.set(runs_w.get() + 1);
    });
    assert_eq!(runs.get(), 1);

    batch(|| {
        set_a.set(1);
        set_b.set(2);
    });
    flush_sync();
    assert_eq!(runs.get(), 2, "batch should coalesce two writes into one rerun");

    // Outside a batch, two writes still coalesce into one rerun because
    // they both land in the same HashSet queue.
    set_a.set(3);
    set_b.set(4);
    flush_sync();
    assert_eq!(runs.get(), 3);
}

#[wasm_bindgen_test]
fn computed_is_lazy_and_cached() {
    setup();
    let (a, set_a) = signal(2_i32);
    let calls = Rc::new(Cell::new(0));
    let calls_w = calls.clone();
    let a_c = a.clone();
    let doubled = computed(move || {
        calls_w.set(calls_w.get() + 1);
        a_c.get() * 2
    });

    // Not computed yet — lazy.
    assert_eq!(calls.get(), 0);

    assert_eq!(doubled.get(), 4);
    assert_eq!(calls.get(), 1);

    // Second read without dep change — cached.
    assert_eq!(doubled.get(), 4);
    assert_eq!(calls.get(), 1);

    // Dep changed — next read recomputes.
    set_a.set(10);
    assert_eq!(doubled.get(), 20);
    assert_eq!(calls.get(), 2);
}

#[wasm_bindgen_test]
fn computed_propagates_through_effect() {
    setup();
    let (a, set_a) = signal(1_i32);
    let a_c = a.clone();
    let sq = Rc::new(computed(move || a_c.get() * a_c.get()));

    let seen = Rc::new(Cell::new(0));
    let seen_w = seen.clone();
    let sq_for_effect = sq.clone();
    effect(move || {
        seen_w.set(sq_for_effect.get());
    });
    assert_eq!(seen.get(), 1);

    set_a.set(4);
    flush_sync();
    assert_eq!(seen.get(), 16);
}

#[wasm_bindgen_test]
fn watch_fires_on_distinct_values_only() {
    setup();
    let sig = rw_signal(5_i32);
    let hits = Rc::new(Cell::new(0));
    let last_old: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));
    let last_new = Rc::new(Cell::new(0));

    let hits_w = hits.clone();
    let old_w = last_old.clone();
    let new_w = last_new.clone();
    let sig_c = sig.clone();
    watch(
        move || sig_c.get(),
        move |next, prev| {
            hits_w.set(hits_w.get() + 1);
            old_w.set(prev.copied());
            new_w.set(*next);
        },
    );
    // Initial run: fires once with prev=None.
    assert_eq!(hits.get(), 1);
    assert_eq!(last_old.get(), None);
    assert_eq!(last_new.get(), 5);

    sig.set(5); // same value — no fire
    flush_sync();
    assert_eq!(hits.get(), 1);

    sig.set(7);
    flush_sync();
    assert_eq!(hits.get(), 2);
    assert_eq!(last_old.get(), Some(5));
    assert_eq!(last_new.get(), 7);
}

#[wasm_bindgen_test]
fn set_skips_trigger_when_value_unchanged() {
    setup();
    let (s, setter) = signal(0_i32);
    let runs = Rc::new(Cell::new(0));
    let runs_w = runs.clone();
    let s_c = s.clone();
    effect(move || {
        let _ = s_c.get();
        runs_w.set(runs_w.get() + 1);
    });
    assert_eq!(runs.get(), 1);

    // Same value → no trigger.
    setter.set(0);
    flush_sync();
    assert_eq!(runs.get(), 1, "same-value set must not re-run subscribers");

    // Different value → triggers.
    setter.set(1);
    flush_sync();
    assert_eq!(runs.get(), 2);

    // Same value again → no trigger.
    setter.set(1);
    flush_sync();
    assert_eq!(runs.get(), 2);
}

#[wasm_bindgen_test]
fn set_force_always_triggers_even_when_unchanged() {
    setup();
    let (s, setter) = signal(0_i32);
    let runs = Rc::new(Cell::new(0));
    let runs_w = runs.clone();
    let s_c = s.clone();
    effect(move || {
        let _ = s_c.get();
        runs_w.set(runs_w.get() + 1);
    });
    assert_eq!(runs.get(), 1);

    setter.set_force(0);
    flush_sync();
    assert_eq!(runs.get(), 2, "set_force must re-fire on same value");

    setter.set_force(0);
    flush_sync();
    assert_eq!(runs.get(), 3);
}

#[wasm_bindgen_test]
fn update_always_triggers_even_when_unchanged() {
    setup();
    let (s, setter) = signal(0_i32);
    let runs = Rc::new(Cell::new(0));
    let runs_w = runs.clone();
    let s_c = s.clone();
    effect(move || {
        let _ = s_c.get();
        runs_w.set(runs_w.get() + 1);
    });
    assert_eq!(runs.get(), 1);

    // Update that doesn't mutate still fires — closure-based
    // mutation can't be proven identity-free without a clone.
    setter.update(|v| {
        let _ = v;
    });
    flush_sync();
    assert_eq!(runs.get(), 2, "update must always trigger");
}

// ── devtools hook tests (PR B) ────────────────────────────────────
//
// Verify the cfg-gated tap points in reactive/signal/computed fire
// the registered handlers. These only build when the `devtools`
// feature is on (default). When the feature is off the module
// `pocopine_core::devtools` doesn't exist and the tests skip.

#[cfg(feature = "devtools")]
mod devtools_hooks {
    use super::*;
    use pocopine_core::devtools::{hooks, ring};
    use std::rc::Rc;

    fn teardown() {
        hooks::_reset();
        ring::_clear();
    }

    #[wasm_bindgen_test]
    fn on_signal_trigger_fires_on_set_when_changed() {
        setup();
        teardown();
        let hits = Rc::new(Cell::new(0u32));
        let hits_w = hits.clone();
        hooks::set_on_signal_trigger(Rc::new(move |_id| {
            hits_w.set(hits_w.get() + 1);
        }));

        let (_s, setter) = signal(0_i32);
        setter.set(1);
        assert_eq!(hits.get(), 1, "first set fires the signal hook");

        // Same-value set should NOT fire — the equality guard in
        // Setter::set skips the trigger entirely.
        setter.set(1);
        assert_eq!(hits.get(), 1, "same-value set must not fire the hook");

        setter.set(2);
        assert_eq!(hits.get(), 2);

        teardown();
    }

    #[wasm_bindgen_test]
    fn on_effect_run_fires_at_least_once() {
        setup();
        teardown();
        let hits = Rc::new(Cell::new(0u32));
        let hits_w = hits.clone();
        hooks::set_on_effect_run(Rc::new(move |_id, _scope, _dur| {
            hits_w.set(hits_w.get() + 1);
        }));

        let (s, setter) = signal(0_i32);
        let s_c = s.clone();
        effect(move || {
            let _ = s_c.get();
        });
        // Effect runs immediately on registration.
        assert!(hits.get() >= 1);

        let before = hits.get();
        setter.set(42);
        flush_sync();
        assert!(hits.get() > before, "effect rerun must fire the hook again");

        teardown();
    }

    #[wasm_bindgen_test]
    fn timeline_push_effect_run_caps_at_cap() {
        setup();
        teardown();
        // Install the default handler (ring push) directly.
        hooks::set_on_effect_run(Rc::new(|id, scope, dur| {
            ring::push_effect_run(id, scope, dur);
        }));

        let (s, setter) = signal(0_i32);
        let s_c = s.clone();
        effect(move || {
            let _ = s_c.get();
        });

        // Fire enough sets to overflow the ring (CAP = 200).
        for i in 0..250 {
            setter.set(i);
            flush_sync();
        }
        let len = ring::len();
        assert!(len > 0, "ring must have captured events");
        assert!(len <= 200, "ring must cap at CAP (200), got {len}");

        teardown();
    }
}
