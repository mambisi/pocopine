//! Deep (dotted) write-back through `write_path_with` /
//! assignment expressions — the RFC-024 §7 follow-up: a nested
//! `pp-model` / assign path mutates the projection snapshot and
//! the framework surfaces the write by writing the whole field
//! back through the scoped writer, so Rust state updates and
//! effects re-run exactly like a flat write. Runs under
//! `wasm-pack test --node`.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use pocopine_core::{Scope, effect, flush_sync, register_store_scope, set_auto_flush};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

fn setup() {
    // spawn_local's microtask host isn't reliable under
    // `wasm-pack test --node`; drive flushes manually.
    set_auto_flush(false);
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Limits {
    max: i32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Settings {
    invite_policy: String,
    limits: Limits,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Item {
    label: String,
    done: bool,
}

struct NestedState {
    settings: Settings,
    items: Vec<Item>,
    count: i32,
}

impl Default for NestedState {
    fn default() -> Self {
        Self {
            settings: Settings {
                invite_policy: "members".into(),
                limits: Limits { max: 10 },
            },
            items: vec![
                Item {
                    label: "one".into(),
                    done: false,
                },
                Item {
                    label: "two".into(),
                    done: true,
                },
            ],
            count: 0,
        }
    }
}

impl pocopine_core::ComponentState for NestedState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "settings" => serde_wasm_bindgen::to_value(&self.settings).unwrap_or(JsValue::NULL),
            "items" => serde_wasm_bindgen::to_value(&self.items).unwrap_or(JsValue::NULL),
            "count" => JsValue::from_f64(self.count as f64),
            _ => JsValue::UNDEFINED,
        }
    }
    fn set(&mut self, key: &str, value: JsValue) {
        match key {
            "settings" => {
                if let Ok(v) = serde_wasm_bindgen::from_value(value) {
                    self.settings = v;
                }
            }
            "items" => {
                if let Ok(v) = serde_wasm_bindgen::from_value(value) {
                    self.items = v;
                }
            }
            "count" => self.count = value.as_f64().unwrap_or_default() as i32,
            _ => {}
        }
    }
    fn keys(&self) -> &'static [&'static str] {
        &["settings", "items", "count"]
    }
    fn invoke(&mut self, _key: &str, _args: &js_sys::Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

/// Register a fresh `NestedState` store under `name` (names must be
/// unique per test — the registry is instance-global).
fn register(name: &'static str) -> Rc<RefCell<NestedState>> {
    let state = Rc::new(RefCell::new(NestedState::default()));
    let scope = Scope::new(state.clone());
    register_store_scope(name, scope);
    state
}

/// Effect that mirrors a `$store`-rooted path into a cell, the way
/// a template binding would.
fn observe(path: &'static str) -> Rc<RefCell<JsValue>> {
    let seen = Rc::new(RefCell::new(JsValue::UNDEFINED));
    let seen_w = seen.clone();
    effect(move || {
        *seen_w.borrow_mut() =
            pocopine_core::path::resolve_path_with(&JsValue::UNDEFINED, None, path);
    });
    seen
}

#[wasm_bindgen_test]
fn store_deep_write_lands_in_rust_state_and_triggers() {
    setup();
    let state = register("dwa");
    let seen = observe("$store.dwa.settings.invite_policy");
    assert_eq!(seen.borrow().as_string().as_deref(), Some("members"));

    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        None,
        "$store.dwa.settings.invite_policy",
        &JsValue::from_str("admins"),
    );
    assert!(ok, "deep store write reported dropped");
    flush_sync();

    assert_eq!(state.borrow().settings.invite_policy, "admins");
    assert_eq!(seen.borrow().as_string().as_deref(), Some("admins"));
    // Sibling leaves of the written field survive the round-trip.
    assert_eq!(state.borrow().settings.limits.max, 10);
}

#[wasm_bindgen_test]
fn store_deep_write_three_levels() {
    setup();
    let state = register("dwb");
    let seen = observe("$store.dwb.settings.limits.max");
    assert_eq!(seen.borrow().as_f64(), Some(10.0));

    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        None,
        "$store.dwb.settings.limits.max",
        &JsValue::from_f64(42.0),
    );
    assert!(ok);
    flush_sync();

    assert_eq!(state.borrow().settings.limits.max, 42);
    assert_eq!(seen.borrow().as_f64(), Some(42.0));
    assert_eq!(state.borrow().settings.invite_policy, "members");
}

#[wasm_bindgen_test]
fn store_deep_write_vec_element() {
    setup();
    let state = register("dwc");
    let seen = observe("$store.dwc.items.0.label");
    assert_eq!(seen.borrow().as_string().as_deref(), Some("one"));

    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        None,
        "$store.dwc.items.0.label",
        &JsValue::from_str("renamed"),
    );
    assert!(ok);
    flush_sync();

    assert_eq!(state.borrow().items[0].label, "renamed");
    assert_eq!(seen.borrow().as_string().as_deref(), Some("renamed"));
    // The untouched element round-trips intact.
    assert_eq!(state.borrow().items[1].label, "two");
    assert!(state.borrow().items[1].done);
}

#[wasm_bindgen_test]
fn store_flat_write_unchanged() {
    setup();
    let state = register("dwd");
    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        None,
        "$store.dwd.count",
        &JsValue::from_f64(5.0),
    );
    assert!(ok);
    flush_sync();
    assert_eq!(state.borrow().count, 5);
}

#[wasm_bindgen_test]
fn store_deep_write_through_missing_middle_is_dropped() {
    setup();
    let state = register("dwe");
    let before = state.borrow().settings.clone();

    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        None,
        "$store.dwe.settings.missing.leaf",
        &JsValue::from_f64(1.0),
    );
    assert!(!ok, "write through a missing segment must be dropped");
    assert_eq!(state.borrow().settings, before);

    // A bare store path has no field to write.
    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        None,
        "$store.dwe",
        &JsValue::from_f64(1.0),
    );
    assert!(!ok);
}

#[wasm_bindgen_test]
fn component_field_deep_write_via_access() {
    setup();
    let state = Rc::new(RefCell::new(NestedState::default()));
    let scope = Scope::new(state.clone());
    let access = pocopine_core::scope::scoped_root_reader(scope.id).expect("live scope");

    let seen = Rc::new(RefCell::new(JsValue::UNDEFINED));
    let seen_w = seen.clone();
    let access_r = access.clone();
    effect(move || {
        *seen_w.borrow_mut() = pocopine_core::path::resolve_path_with(
            &JsValue::UNDEFINED,
            Some(&access_r),
            "settings.limits.max",
        );
    });
    assert_eq!(seen.borrow().as_f64(), Some(10.0));

    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        Some(&access),
        "settings.limits.max",
        &JsValue::from_f64(7.0),
    );
    assert!(ok);
    flush_sync();

    assert_eq!(state.borrow().settings.limits.max, 7);
    assert_eq!(seen.borrow().as_f64(), Some(7.0));
}

#[wasm_bindgen_test]
fn assignment_expression_deep_write_surfaces() {
    setup();
    // Store-rooted assign, no access — the magic branch.
    let state = register("dwf");
    let assign =
        pocopine_core::expr::parse_cached("$store.dwf.settings.limits.max = 9").expect("parses");
    pocopine_core::expr::evaluate_with(&assign, &JsValue::UNDEFINED, None);
    flush_sync();
    assert_eq!(state.borrow().settings.limits.max, 9);

    // Component-field assign through the scoped access — the
    // plain branch, same core.
    let local = Rc::new(RefCell::new(NestedState::default()));
    let scope = Scope::new(local.clone());
    let access = pocopine_core::scope::scoped_root_reader(scope.id).expect("live scope");
    let assign = pocopine_core::expr::parse_cached("settings.limits.max = 3").expect("parses");
    pocopine_core::expr::evaluate_with(&assign, &JsValue::UNDEFINED, Some(&access));
    flush_sync();
    assert_eq!(local.borrow().settings.limits.max, 3);
}
