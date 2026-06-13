//! Unit tests for the reactive core: effects, cleanups, batching,
//! signals, computed, watch. Runs under `wasm-pack test --node`.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Reflect;
use pocopine_core::{
    batch, computed, effect, flush_sync, on_cleanup, on_scope_unmount_for, release, run_now,
    rw_signal, set_auto_flush, signal, spawn_for_scope, spawn_latest, spawn_latest_for_scope,
    spawn_scoped, watch, watch_scope_field_now, FieldHandle, Handle, Scope,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
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
    assert_eq!(
        runs.get(),
        2,
        "batch should coalesce two writes into one rerun"
    );

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

#[derive(Default, Serialize, Deserialize)]
struct TestScopeState {
    value: String,
}

impl pocopine_core::ComponentState for TestScopeState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "value" => JsValue::from_str(&self.value),
            _ => JsValue::UNDEFINED,
        }
    }

    fn set(&mut self, key: &str, value: JsValue) {
        if key == "value" {
            self.value = value.as_string().unwrap_or_default();
        }
    }

    fn keys(&self) -> &'static [&'static str] {
        &["value"]
    }

    fn invoke(&mut self, _key: &str, _args: &js_sys::Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

#[wasm_bindgen_test]
fn watch_scope_field_now_reads_existing_value_on_install() {
    setup();

    let state = Rc::new(std::cell::RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state);
    let proxy = scope.into_proxy();
    let _ = Reflect::set(
        &proxy,
        &JsValue::from_str("value"),
        &JsValue::from_str("2024-06-15"),
    );

    let hits = Rc::new(Cell::new(0));
    let seen = Rc::new(std::cell::RefCell::new(String::new()));
    let hits_w = hits.clone();
    let seen_w = seen.clone();
    watch_scope_field_now::<String, _>(scope.id, "value", move |next, prev| {
        hits_w.set(hits_w.get() + 1);
        assert!(prev.is_none(), "initial install should report prev=None");
        *seen_w.borrow_mut() = next.clone();
    });

    assert_eq!(hits.get(), 1);
    assert_eq!(seen.borrow().as_str(), "2024-06-15");
}

// RFC-097 — FieldHandle::set writes one field with NO dirty sweep.
#[wasm_bindgen_test]
fn field_handle_set_writes_one_field_without_a_sweep() {
    setup();
    let state = Rc::new(RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state.clone());
    let me: Handle<TestScopeState> = Handle::new(state, scope.id);
    let fh: FieldHandle<String> = FieldHandle::__new(scope.id, "value");

    // An effect subscribed to exactly this field.
    let runs = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(String::new()));
    {
        let runs = runs.clone();
        let seen = seen.clone();
        effect(move || {
            runs.set(runs.get() + 1);
            *seen.borrow_mut() = fh.get();
        });
    }
    assert_eq!(runs.get(), 1, "effect runs once on install");

    // A swept Handle::update DOES fingerprint (proves the counter is live).
    let fp0 = pocopine_core::scope::fingerprint_count();
    me.update(|s| s.value = "via_update".into());
    flush_sync();
    let fp1 = pocopine_core::scope::fingerprint_count();
    assert!(fp1 > fp0, "a swept update must run fingerprints");
    assert_eq!(runs.get(), 2);
    assert_eq!(seen.borrow().as_str(), "via_update");

    // FieldHandle::set must run ZERO fingerprints (no sweep) yet still
    // trigger exactly this field's effect.
    fh.set("via_set".into());
    flush_sync();
    let fp2 = pocopine_core::scope::fingerprint_count();
    assert_eq!(fp2, fp1, "FieldHandle::set must run no fingerprints");
    assert_eq!(runs.get(), 3, "the field's effect re-ran exactly once");
    assert_eq!(seen.borrow().as_str(), "via_set");
    assert_eq!(fh.get(), "via_set".to_string(), "get reflects the write");
}

// RFC-097 — FieldHandle::update is a read-modify-write of one field.
#[wasm_bindgen_test]
fn field_handle_update_read_modify_writes_one_field() {
    setup();
    let state = Rc::new(RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state);
    let fh: FieldHandle<String> = FieldHandle::__new(scope.id, "value");
    fh.set("ab".into());
    flush_sync();

    let fp0 = pocopine_core::scope::fingerprint_count();
    fh.update(|v| v.push('c'));
    flush_sync();
    let fp1 = pocopine_core::scope::fingerprint_count();

    assert_eq!(fh.get(), "abc".to_string());
    assert_eq!(fp1, fp0, "FieldHandle::update must run no fingerprints");
}

#[wasm_bindgen_test]
fn spawn_scoped_cancels_when_scope_is_removed() {
    setup();

    let state = Rc::new(std::cell::RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state);
    let handle =
        pocopine_core::scope::with_current_scope_id(scope.id, || spawn_scoped(async move {}));

    assert!(!handle.is_cancelled());
    Scope::remove(scope.id);
    assert!(handle.is_cancelled());
}

#[wasm_bindgen_test]
fn spawn_for_scope_returns_cancelled_handle_when_scope_is_gone() {
    setup();

    let state = Rc::new(std::cell::RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state);
    let scope_id = scope.id;
    Scope::remove(scope_id);

    let handle = spawn_for_scope(scope_id, async move {});

    assert!(handle.is_cancelled());
}

#[wasm_bindgen_test]
fn spawn_latest_cancels_previous_task_in_same_slot() {
    setup();

    let state = Rc::new(std::cell::RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state);
    let (first, second) = pocopine_core::scope::with_current_scope_id(scope.id, || {
        let first = spawn_latest("search", async move {});
        let second = spawn_latest("search", async move {});
        (first, second)
    });

    assert!(
        first.is_cancelled(),
        "older latest-wins task should be cancelled"
    );
    assert!(
        !second.is_cancelled(),
        "newest latest-wins task should stay active until scope teardown"
    );

    Scope::remove(scope.id);
    assert!(second.is_cancelled());
}

#[wasm_bindgen_test]
fn spawn_latest_for_scope_cancels_previous_task_in_same_slot() {
    setup();

    let state = Rc::new(std::cell::RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state);
    let first = spawn_latest_for_scope(scope.id, "search", async move {});
    let second = spawn_latest_for_scope(scope.id, "search", async move {});

    assert!(
        first.is_cancelled(),
        "older latest-wins task should be cancelled"
    );
    assert!(
        !second.is_cancelled(),
        "newest latest-wins task should stay active until scope teardown"
    );

    Scope::remove(scope.id);
    assert!(second.is_cancelled());
}

#[wasm_bindgen_test]
fn on_scope_unmount_for_runs_when_scope_is_removed() {
    setup();

    let state = Rc::new(std::cell::RefCell::new(TestScopeState::default()));
    let scope = Scope::new(state);
    let ran = Rc::new(Cell::new(false));
    let ran_for_cleanup = ran.clone();
    on_scope_unmount_for(scope.id, move || ran_for_cleanup.set(true));

    Scope::remove(scope.id);

    assert!(ran.get());
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

// ─── RFC-095 W2 — per-field dirty sweep ─────────────────────────

/// Two independent fields with real fingerprints. The dirty sweep
/// must trigger only the effects subscribed to the field a
/// handler actually changed.
#[derive(Default, Serialize, Deserialize)]
struct DirtyState {
    a: u32,
    b: u32,
}

impl pocopine_core::ComponentState for DirtyState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "a" => JsValue::from_f64(self.a as f64),
            "b" => JsValue::from_f64(self.b as f64),
            _ => JsValue::UNDEFINED,
        }
    }
    fn set(&mut self, key: &str, value: JsValue) {
        let v = value.as_f64().unwrap_or_default() as u32;
        match key {
            "a" => self.a = v,
            "b" => self.b = v,
            _ => {}
        }
    }
    fn keys(&self) -> &'static [&'static str] {
        &["a", "b"]
    }
    fn field_fingerprint(&self, key: &str) -> Option<u64> {
        match key {
            "a" => pocopine_core::fingerprint::fingerprint(&self.a),
            "b" => pocopine_core::fingerprint::fingerprint(&self.b),
            _ => None,
        }
    }
    fn invoke(&mut self, key: &str, _args: &js_sys::Array) -> JsValue {
        match key {
            "bump_a" => self.a += 1,
            "noop" => {}
            _ => {}
        }
        JsValue::UNDEFINED
    }
}

/// Same shape, but fingerprints are unknown — the sweep must fall
/// back to triggering every tracked key (pre-W2 behavior).
#[derive(Default, Serialize, Deserialize)]
struct OpaqueState {
    a: u32,
    b: u32,
}

impl pocopine_core::ComponentState for OpaqueState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "a" => JsValue::from_f64(self.a as f64),
            "b" => JsValue::from_f64(self.b as f64),
            _ => JsValue::UNDEFINED,
        }
    }
    fn set(&mut self, _key: &str, _value: JsValue) {}
    fn keys(&self) -> &'static [&'static str] {
        &["a", "b"]
    }
    // field_fingerprint: default None — "unknown".
    fn invoke(&mut self, key: &str, _args: &js_sys::Array) -> JsValue {
        if key == "bump_a" {
            self.a += 1;
        }
        JsValue::UNDEFINED
    }
}

/// Track one effect per field; return (hits_a, hits_b) counters.
fn install_field_effects(proxy: &JsValue) -> (Rc<Cell<u32>>, Rc<Cell<u32>>) {
    let hits_a = Rc::new(Cell::new(0));
    let hits_b = Rc::new(Cell::new(0));
    let (p, h) = (proxy.clone(), hits_a.clone());
    effect(move || {
        let _ = Reflect::get(&p, &JsValue::from_str("a"));
        h.set(h.get() + 1);
    });
    let (p, h) = (proxy.clone(), hits_b.clone());
    effect(move || {
        let _ = Reflect::get(&p, &JsValue::from_str("b"));
        h.set(h.get() + 1);
    });
    (hits_a, hits_b)
}

#[wasm_bindgen_test]
fn dirty_sweep_triggers_only_changed_fields() {
    setup();
    let scope = Scope::new(Rc::new(std::cell::RefCell::new(DirtyState::default())));
    let proxy = scope.into_proxy();
    let (hits_a, hits_b) = install_field_effects(&proxy);
    assert_eq!((hits_a.get(), hits_b.get()), (1, 1), "initial runs");

    scope.invoke("bump_a", &js_sys::Array::new());
    flush_sync();
    assert_eq!(hits_a.get(), 2, "effect on changed field `a` must re-run");
    assert_eq!(
        hits_b.get(),
        1,
        "effect on untouched field `b` must NOT re-run",
    );
}

#[wasm_bindgen_test]
fn dirty_sweep_no_change_no_trigger() {
    setup();
    let scope = Scope::new(Rc::new(std::cell::RefCell::new(DirtyState::default())));
    let proxy = scope.into_proxy();
    let (hits_a, hits_b) = install_field_effects(&proxy);

    scope.invoke("noop", &js_sys::Array::new());
    flush_sync();
    assert_eq!(
        (hits_a.get(), hits_b.get()),
        (1, 1),
        "a handler that mutates nothing must trigger nothing",
    );
}

#[wasm_bindgen_test]
fn dirty_sweep_unknown_fingerprints_fall_back_to_full_trigger() {
    setup();
    let scope = Scope::new(Rc::new(std::cell::RefCell::new(OpaqueState::default())));
    let proxy = scope.into_proxy();
    let (hits_a, hits_b) = install_field_effects(&proxy);

    scope.invoke("bump_a", &js_sys::Array::new());
    flush_sync();
    assert_eq!(
        (hits_a.get(), hits_b.get()),
        (2, 2),
        "unknown fingerprints must conservatively re-run every tracked effect",
    );
}

#[wasm_bindgen_test]
fn handle_update_triggers_only_changed_fields() {
    setup();
    let state = Rc::new(std::cell::RefCell::new(DirtyState::default()));
    let scope = Scope::new(state.clone());
    let proxy = scope.into_proxy();
    let (hits_a, hits_b) = install_field_effects(&proxy);

    let handle = pocopine_core::Handle::new(state, scope.id);
    handle.update(|s| s.b = 99);
    flush_sync();
    assert_eq!(
        hits_a.get(),
        1,
        "Handle::update must not re-run effects on untouched field `a`",
    );
    assert_eq!(hits_b.get(), 2, "effect on changed field `b` must re-run");

    // And the no-op update: nothing re-runs.
    handle.update(|_| {});
    flush_sync();
    assert_eq!((hits_a.get(), hits_b.get()), (1, 2));
}

/// The stale-cache half of the sweep: an unchanged field's cached
/// JsValue survives the handler (no re-serialization), while the
/// changed field's slot is dropped so the next read is fresh.
#[wasm_bindgen_test]
fn dirty_sweep_keeps_unchanged_field_cache_and_drops_changed() {
    setup();
    let scope = Scope::new(Rc::new(std::cell::RefCell::new(DirtyState::default())));
    let proxy = scope.into_proxy();
    let (_hits_a, _hits_b) = install_field_effects(&proxy);

    scope.invoke("bump_a", &js_sys::Array::new());
    flush_sync();
    // Changed field reads fresh through the (invalidated) cache.
    let a = Reflect::get(&proxy, &JsValue::from_str("a")).unwrap();
    assert_eq!(a.as_f64(), Some(1.0), "changed field must read fresh value");
    let b = Reflect::get(&proxy, &JsValue::from_str("b")).unwrap();
    assert_eq!(b.as_f64(), Some(0.0), "unchanged field stays readable");
}

// ─── RFC-095 W1 — proxy-free root reads ─────────────────────────

/// `evaluate_with` + `scoped_root_reader` must (a) resolve field
/// values without touching the proxy and (b) register the same
/// dependencies the proxy path would — the effect re-runs when
/// the field triggers.
#[wasm_bindgen_test]
fn scoped_root_reader_resolves_and_tracks() {
    setup();
    let scope = Scope::new(Rc::new(std::cell::RefCell::new(DirtyState::default())));
    let reader = pocopine_core::scope::scoped_root_reader(scope.id).expect("live scope");

    let ast = pocopine_core::expr::parse_cached("a").expect("parses");
    let hits = Rc::new(Cell::new(0));
    let (h, r) = (hits.clone(), reader.clone());
    let seen = Rc::new(Cell::new(-1.0));
    let seen_c = seen.clone();
    // Proxy deliberately UNDEFINED: resolution must come from the
    // reader alone — a fallback to the proxy would read undefined.
    effect(move || {
        let v = pocopine_core::expr::evaluate_with(&ast, &JsValue::UNDEFINED, Some(&r));
        seen_c.set(v.as_f64().unwrap_or(f64::NAN));
        h.set(h.get() + 1);
    });
    assert_eq!(hits.get(), 1);
    assert_eq!(seen.get(), 0.0, "reader must resolve the field value");

    scope.invoke("bump_a", &js_sys::Array::new());
    flush_sync();
    assert_eq!(
        hits.get(),
        2,
        "reader read must subscribe like a proxy read"
    );
    assert_eq!(seen.get(), 1.0, "re-run must observe the new value");
}

// ─── RFC-095 W0 — differential fuzz harness ─────────────────────
//
// The correctness gate for the fine-grained pipeline (W1 scoped
// reads + W2 dirty sweep + W3a unified graph): drive random
// mutation sequences through every write path the framework has
// (handler invoke, Handle::update, proxy set-trap) and after
// every flush compare what each effect last observed against a
// re-evaluate-from-state oracle. Deterministic LCG seeds — a
// failure reproduces from its seed printout.

#[derive(Default, Serialize, Deserialize)]
struct FuzzState {
    a: i32,
    b: i32,
    c: i32,
    items: Vec<i32>,
}

impl pocopine_core::ComponentState for FuzzState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "a" => JsValue::from_f64(self.a as f64),
            "b" => JsValue::from_f64(self.b as f64),
            "c" => JsValue::from_f64(self.c as f64),
            "items" => serde_wasm_bindgen::to_value(&self.items).unwrap_or(JsValue::NULL),
            _ => JsValue::UNDEFINED,
        }
    }
    fn set(&mut self, key: &str, value: JsValue) {
        let n = value.as_f64().unwrap_or_default() as i32;
        match key {
            "a" => self.a = n,
            "b" => self.b = n,
            "c" => self.c = n,
            _ => {}
        }
    }
    fn keys(&self) -> &'static [&'static str] {
        &["a", "b", "c", "items"]
    }
    fn field_fingerprint(&self, key: &str) -> Option<u64> {
        match key {
            "a" => pocopine_core::fingerprint::fingerprint(&self.a),
            "b" => pocopine_core::fingerprint::fingerprint(&self.b),
            "c" => pocopine_core::fingerprint::fingerprint(&self.c),
            "items" => pocopine_core::fingerprint::fingerprint(&self.items),
            _ => None,
        }
    }
    fn invoke(&mut self, key: &str, args: &js_sys::Array) -> JsValue {
        let arg = args.get(0).as_f64().unwrap_or_default() as i32;
        match key {
            "set_a" => self.a = arg,
            "set_b" => self.b = arg,
            "bump_all" => {
                self.a += 1;
                self.b += 1;
                self.c += 1;
            }
            "push_item" => self.items.push(arg),
            "pop_item" => {
                self.items.pop();
            }
            "write_same_a" => {
                // Mutate-and-restore: net no change. The sweep
                // must NOT trigger anything for this.
                self.a += 1;
                self.a -= 1;
            }
            "noop" => {}
            _ => {}
        }
        JsValue::UNDEFINED
    }
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self, bound: u64) -> u64 {
        // Numerical Recipes LCG — deterministic across runs.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) % bound
    }
}

#[wasm_bindgen_test]
fn differential_fuzz_fine_grained_matches_oracle() {
    setup();
    for seed in [1u64, 7, 42, 1234, 99999] {
        let state = Rc::new(std::cell::RefCell::new(FuzzState::default()));
        let scope = Scope::new(state.clone());
        let proxy = scope.into_proxy();
        let handle = pocopine_core::Handle::new(state.clone(), scope.id);

        // Observers: each records the value it last computed.
        // Mix of single-field, cross-field, and collection reads.
        let read = |p: &JsValue, k: &str| {
            Reflect::get(p, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
        };
        let obs_a = Rc::new(Cell::new(f64::NAN));
        let obs_sum = Rc::new(Cell::new(f64::NAN));
        let obs_len = Rc::new(Cell::new(f64::NAN));
        let runs = Rc::new(Cell::new(0u32));
        {
            let (p, o) = (proxy.clone(), obs_a.clone());
            let r = runs.clone();
            effect(move || {
                o.set(read(&p, "a").as_f64().unwrap_or(f64::NAN));
                r.set(r.get() + 1);
            });
        }
        {
            let (p, o) = (proxy.clone(), obs_sum.clone());
            effect(move || {
                let a = read(&p, "a").as_f64().unwrap_or(0.0);
                let b = read(&p, "b").as_f64().unwrap_or(0.0);
                o.set(a + b);
            });
        }
        {
            let (p, o) = (proxy.clone(), obs_len.clone());
            effect(move || {
                let items = read(&p, "items");
                let len = js_sys::Array::from(&items).length() as f64;
                o.set(len);
            });
        }

        // RFC-096 S1 — the write mirror under fuzz: template-style
        // assignments evaluated with the scoped access and a
        // deliberately-UNDEFINED proxy (a fallback would explode),
        // plus direct scoped-writer prop writes.
        let access = pocopine_core::scope::scoped_root_reader(scope.id).expect("live scope");
        let assign_b = pocopine_core::expr::parse_cached("b = a + 1").expect("parses");

        let mut rng = Lcg(seed);
        let mut runs_before_noop;
        for step in 0..200 {
            let op = rng.next(11);
            let arg = rng.next(100) as f64;
            match op {
                0 => {
                    scope.invoke("set_a", &js_sys::Array::of1(&JsValue::from_f64(arg)));
                }
                1 => {
                    scope.invoke("set_b", &js_sys::Array::of1(&JsValue::from_f64(arg)));
                }
                2 => {
                    scope.invoke("bump_all", &js_sys::Array::new());
                }
                3 => {
                    scope.invoke("push_item", &js_sys::Array::of1(&JsValue::from_f64(arg)));
                }
                4 => {
                    scope.invoke("pop_item", &js_sys::Array::new());
                }
                5 => {
                    // Template-style write through the set trap.
                    let _ = Reflect::set(&proxy, &JsValue::from_str("a"), &JsValue::from_f64(arg));
                }
                6 => {
                    handle.update(|s| s.c = arg as i32);
                }
                7 => {
                    // Precision probe: a handler that nets no change
                    // must not re-run anything.
                    flush_sync();
                    runs_before_noop = runs.get();
                    scope.invoke("write_same_a", &js_sys::Array::new());
                    flush_sync();
                    assert_eq!(
                        runs.get(),
                        runs_before_noop,
                        "seed {seed} step {step}: no-net-change handler re-ran an effect",
                    );
                }
                8 => {
                    // Template assignment through the write
                    // mirror — `@click="b = a + 1"` with no proxy.
                    let _ = pocopine_core::expr::evaluate_with(
                        &assign_b,
                        &JsValue::UNDEFINED,
                        Some(&access),
                    );
                }
                9 => {
                    // Parent-style prop write through the scoped
                    // writer (the pp-bind/pp-model path).
                    pocopine_core::scope::write_field(scope.id, "c", &JsValue::from_f64(arg));
                }
                _ => {
                    scope.invoke("noop", &js_sys::Array::new());
                }
            }
            flush_sync();

            // Oracle: recompute every observed value from Rust
            // state and compare with what the effects last wrote.
            let s = state.borrow();
            assert_eq!(
                obs_a.get(),
                s.a as f64,
                "seed {seed} step {step} op {op}: obs_a diverged from state.a",
            );
            assert_eq!(
                obs_sum.get(),
                (s.a + s.b) as f64,
                "seed {seed} step {step} op {op}: obs_sum diverged from a+b",
            );
            assert_eq!(
                obs_len.get(),
                s.items.len() as f64,
                "seed {seed} step {step} op {op}: obs_len diverged from items.len()",
            );
        }
    }
}

// ── RFC-098 core-hardening gates ─────────────────────────────────

/// H4 — dispatch + flush visit subscribers in registration order,
/// deterministically (was `HashSet`, varied per run). The observable
/// guarantee that makes a fuzz failure replay from its seed.
#[wasm_bindgen_test]
fn dispatch_runs_effects_in_subscription_order() {
    setup();
    let (s, setter) = signal(0_i32);
    let order: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));
    // Five effects subscribe to `s` in order 0..5 (subscription order
    // == creation order, since each tracks on its initial run).
    for k in 0..5 {
        let order_w = order.clone();
        let s_k = s.clone();
        effect(move || {
            s_k.get();
            order_w.borrow_mut().push(k);
        });
    }
    order.borrow_mut().clear(); // discard the initial-run records
    setter.set(1);
    flush_sync();
    assert_eq!(
        *order.borrow(),
        vec![0, 1, 2, 3, 4],
        "H4: effects must dispatch in registration order"
    );
}

/// H1 + H2 — an effect released by another effect mid-flush is safe:
/// no panic (the snapshot borrow is dropped before bodies run), and
/// the released effect's id resolves to None (never reruns, stale ops
/// are inert).
#[wasm_bindgen_test]
fn release_during_dispatch_is_safe() {
    setup();
    let (s, setter) = signal(0_i32);
    let a_runs = Rc::new(Cell::new(0));
    let b_runs = Rc::new(Cell::new(0));

    // A subscribes first, so it dispatches first (H4). When armed, A's
    // rerun releases B before B's turn in the same flush.
    let target: Rc<Cell<Option<pocopine_core::EffectId>>> = Rc::new(Cell::new(None));
    let a_runs_w = a_runs.clone();
    let s_a = s.clone();
    let target_a = target.clone();
    effect(move || {
        s_a.get();
        a_runs_w.set(a_runs_w.get() + 1);
        if let Some(b) = target_a.take() {
            release(b);
        }
    });

    let b_runs_w = b_runs.clone();
    let s_b = s.clone();
    let b_id = effect(move || {
        s_b.get();
        b_runs_w.set(b_runs_w.get() + 1);
    });

    assert_eq!(a_runs.get(), 1);
    assert_eq!(b_runs.get(), 1); // both ran once on creation

    // Arm the release, then trigger: A reruns and releases B mid-flush.
    target.set(Some(b_id));
    setter.set(1);
    flush_sync();
    assert_eq!(a_runs.get(), 2);
    assert_eq!(
        b_runs.get(),
        1,
        "B must not rerun after release mid-dispatch"
    );

    // The stale id is inert and does not touch A; A still reacts.
    release(b_id); // no-op, no panic
    run_now(b_id); // no-op, no panic
    setter.set(2);
    flush_sync();
    assert_eq!(a_runs.get(), 3);
    assert_eq!(b_runs.get(), 1);
}

/// H3 — a chain of computeds (A→B→C) propagates through the trampoline
/// without recursion: an inline scheduler's downstream trigger lands on
/// the worklist, drained by the outermost dispatch. The reading effect
/// sees the fully-propagated value, each computed recomputes lazily.
#[wasm_bindgen_test]
fn scheduler_triggers_scheduler_three_deep() {
    setup();
    let (src, set_src) = signal(2_i32);

    let src_a = src.clone();
    let a = Rc::new(computed(move || src_a.get() + 1)); // 3
    let a_b = a.clone();
    let b = Rc::new(computed(move || a_b.get() * 2)); // 6
    let b_c = b.clone();
    let c = Rc::new(computed(move || b_c.get() + 10)); // 16

    let seen = Rc::new(Cell::new(0));
    let evals = Rc::new(Cell::new(0));
    let seen_w = seen.clone();
    let evals_w = evals.clone();
    let c_eff = c.clone();
    effect(move || {
        evals_w.set(evals_w.get() + 1);
        seen_w.set(c_eff.get());
    });
    assert_eq!(seen.get(), 16);
    let evals_after_init = evals.get();

    // One source mutation propagates A→B→C→effect in a single dispatch.
    set_src.set(5);
    flush_sync();
    assert_eq!(seen.get(), (5 + 1) * 2 + 10); // 22
    assert_eq!(
        evals.get(),
        evals_after_init + 1,
        "the reading effect must rerun exactly once for a 3-deep chain"
    );
}

/// H2 — releasing an effect frees its slab slot; the next effect reuses
/// it under a bumped generation, so the freed id is distinct and stays
/// inert (ABA safety).
#[wasm_bindgen_test]
fn slab_generation_reuse_rejects_stale_id() {
    setup();
    let stale = effect(|| {});
    release(stale);

    // The next effect reuses the freed slot with generation + 1.
    let (s, setter) = signal(0_i32);
    let runs = Rc::new(Cell::new(0));
    let runs_w = runs.clone();
    let s_c = s.clone();
    let fresh = effect(move || {
        s_c.get();
        runs_w.set(runs_w.get() + 1);
    });
    assert_ne!(
        stale, fresh,
        "reused slot must mint a distinct id (generation bump)"
    );
    assert_eq!(runs.get(), 1);

    // Stale-id operations are no-ops and must not disturb `fresh`.
    release(stale);
    run_now(stale);
    setter.set(1);
    flush_sync();
    assert_eq!(
        runs.get(),
        2,
        "fresh effect at the reused slot still reacts"
    );
}

/// H3 + H4 — when inline schedulers defer downstream signals to the
/// worklist, sibling branches drain FIFO (trigger order). With the old
/// LIFO drain the two branches would queue in reverse; this asserts the
/// registration-order guarantee across a diamond. (Single-signal order
/// is covered by `dispatch_runs_effects_in_subscription_order`.)
#[wasm_bindgen_test]
fn worklist_preserves_fifo_sibling_order() {
    setup();
    let (s, set_s) = signal(0_i32);

    // Two computeds read `s`; created C1 then C2.
    let s_1 = s.clone();
    let c1 = Rc::new(computed(move || s_1.get() + 1));
    let s_2 = s.clone();
    let c2 = Rc::new(computed(move || s_2.get() + 1));

    let order: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));

    // E_x reads C1, E_y reads C2 — created in that order, so `s`'s
    // subscriber order (and thus the worklist push order) is [C1, C2].
    let c1_e = c1.clone();
    let order_x = order.clone();
    effect(move || {
        c1_e.get();
        order_x.borrow_mut().push("x");
    });
    let c2_e = c2.clone();
    let order_y = order.clone();
    effect(move || {
        c2_e.get();
        order_y.borrow_mut().push("y");
    });

    order.borrow_mut().clear(); // discard initial-run records
    set_s.set(5);
    flush_sync();
    // C1 deferred before C2; FIFO drain dispatches C1 first, so E_x
    // queues and runs before E_y. (LIFO would give ["y", "x"].)
    assert_eq!(
        *order.borrow(),
        vec!["x", "y"],
        "cross-branch sibling effects must keep FIFO trigger order"
    );
}
