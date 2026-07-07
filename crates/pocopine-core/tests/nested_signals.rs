//! RFC-112/113 — nested signals: leaf-granular tracking, native
//! deep writes, the trigger lattice, and the path-aware dirty
//! sweep. Fixtures hand-implement `PathAccess` /
//! `ComponentState::{set_path, path_fingerprint}` (what the
//! derives emit) so the runtime semantics are tested without the
//! macro layer. Runs under `wasm-pack test --node`.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use pocopine_core::path_access::PathAccess;
use pocopine_core::{Scope, effect, flush_sync, set_auto_flush};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

fn setup() {
    set_auto_flush(false);
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Settings {
    theme: String,
    size: i32,
}

impl PathAccess for Settings {
    fn path_set(&mut self, path: &[&str], value: &JsValue) -> bool {
        match path {
            [] => pocopine_core::path_access::set_leaf(self, value),
            ["theme"] => pocopine_core::path_access::set_leaf(&mut self.theme, value),
            ["size"] => pocopine_core::path_access::set_leaf(&mut self.size, value),
            _ => false,
        }
    }
    fn path_fingerprint(&self, path: &[&str]) -> Option<u64> {
        match path {
            [] => pocopine_core::fingerprint::fingerprint(self),
            ["theme"] => pocopine_core::fingerprint::fingerprint(&self.theme),
            ["size"] => pocopine_core::fingerprint::fingerprint(&self.size),
            _ => None,
        }
    }
}

/// A nested struct WITHOUT PathAccess reach — exercises the
/// snapshot fallback lane.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct Blob {
    x: i32,
    y: i32,
}

struct NestedState {
    settings: Settings,
    blob: Blob,
    count: i32,
}

impl Default for NestedState {
    fn default() -> Self {
        Self {
            settings: Settings {
                theme: "light".into(),
                size: 14,
            },
            blob: Blob { x: 1, y: 2 },
            count: 0,
        }
    }
}

impl pocopine_core::ComponentState for NestedState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "settings" => serde_wasm_bindgen::to_value(&self.settings).unwrap_or(JsValue::NULL),
            "blob" => serde_wasm_bindgen::to_value(&self.blob).unwrap_or(JsValue::NULL),
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
            "blob" => {
                if let Ok(v) = serde_wasm_bindgen::from_value(value) {
                    self.blob = v;
                }
            }
            "count" => self.count = value.as_f64().unwrap_or_default() as i32,
            _ => {}
        }
    }
    fn keys(&self) -> &'static [&'static str] {
        &["settings", "blob", "count"]
    }
    fn field_fingerprint(&self, key: &str) -> Option<u64> {
        match key {
            "settings" => pocopine_core::fingerprint::fingerprint(&self.settings),
            "blob" => pocopine_core::fingerprint::fingerprint(&self.blob),
            "count" => pocopine_core::fingerprint::fingerprint(&self.count),
            _ => None,
        }
    }
    fn set_path(&mut self, path: &[&str], value: &JsValue) -> bool {
        match path {
            ["settings", rest @ ..] if !rest.is_empty() => self.settings.path_set(rest, value),
            _ => false,
        }
    }
    fn path_fingerprint(&self, path: &[&str]) -> Option<u64> {
        match path {
            ["settings", rest @ ..] if !rest.is_empty() => self.settings.path_fingerprint(rest),
            _ => None,
        }
    }
    fn invoke(&mut self, key: &str, _args: &js_sys::Array) -> JsValue {
        match key {
            "grow" => self.settings.size += 1,
            "retheme" => self.settings.theme = "dark".into(),
            "touch_restore" => {
                self.settings.size += 1;
                self.settings.size -= 1;
            }
            "bump_count" => self.count += 1,
            _ => {}
        }
        JsValue::UNDEFINED
    }
}

struct Fixture {
    state: Rc<RefCell<NestedState>>,
    scope: Scope,
    access: pocopine_core::expr::RootAccess,
}

fn fixture() -> Fixture {
    let state = Rc::new(RefCell::new(NestedState::default()));
    let scope = Scope::new(state.clone());
    let access = pocopine_core::scope::scoped_root_reader(scope.id).expect("live scope");
    Fixture {
        state,
        scope,
        access,
    }
}

/// Effect that reads `path` through the scoped access (the way a
/// template binding does) and counts its runs.
fn observe(fx: &Fixture, path: &'static str) -> (Rc<Cell<u32>>, Rc<RefCell<JsValue>>) {
    let runs = Rc::new(Cell::new(0));
    let seen = Rc::new(RefCell::new(JsValue::UNDEFINED));
    let (runs_w, seen_w) = (runs.clone(), seen.clone());
    let access = fx.access.clone();
    effect(move || {
        runs_w.set(runs_w.get() + 1);
        *seen_w.borrow_mut() =
            pocopine_core::path::resolve_path_with(&JsValue::UNDEFINED, Some(&access), path);
    });
    (runs, seen)
}

#[wasm_bindgen_test]
fn native_leaf_write_isolates_sibling_leaf() {
    setup();
    let fx = fixture();
    let (theme_runs, theme_seen) = observe(&fx, "settings.theme");
    let (size_runs, _) = observe(&fx, "settings.size");
    assert_eq!((theme_runs.get(), size_runs.get()), (1, 1));

    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        Some(&fx.access),
        "settings.theme",
        &JsValue::from_str("dark"),
    );
    assert!(ok);
    flush_sync();

    assert_eq!(fx.state.borrow().settings.theme, "dark");
    assert_eq!(fx.state.borrow().settings.size, 14);
    assert_eq!(theme_runs.get(), 2, "leaf subscriber re-runs");
    assert_eq!(size_runs.get(), 1, "sibling leaf must stay quiet");
    assert_eq!(theme_seen.borrow().as_string().as_deref(), Some("dark"));
}

#[wasm_bindgen_test]
fn container_write_reaches_leaf_subscribers() {
    setup();
    let fx = fixture();
    let (theme_runs, theme_seen) = observe(&fx, "settings.theme");
    let (size_runs, _) = observe(&fx, "settings.size");

    let next = serde_wasm_bindgen::to_value(&Settings {
        theme: "sepia".into(),
        size: 18,
    })
    .unwrap();
    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        Some(&fx.access),
        "settings",
        &next,
    );
    assert!(ok);
    flush_sync();

    assert_eq!(theme_runs.get(), 2, "down-fan reaches theme leaf");
    assert_eq!(size_runs.get(), 2, "down-fan reaches size leaf");
    assert_eq!(theme_seen.borrow().as_string().as_deref(), Some("sepia"));
}

#[wasm_bindgen_test]
fn native_leaf_write_fires_container_subscriber_with_fresh_value() {
    setup();
    let fx = fixture();
    // Whole-container reader (a `#[watch(settings)]`-shaped dep).
    let (root_runs, root_seen) = observe(&fx, "settings");

    pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        Some(&fx.access),
        "settings.size",
        &JsValue::from_f64(99.0),
    );
    flush_sync();

    assert_eq!(root_runs.get(), 2, "ancestor subscriber fires");
    let size = js_sys::Reflect::get(&root_seen.borrow(), &JsValue::from_str("size"))
        .unwrap()
        .as_f64();
    assert_eq!(size, Some(99.0), "container projection was invalidated");
}

#[wasm_bindgen_test]
fn handler_sweep_is_leaf_precise() {
    setup();
    let fx = fixture();
    let (theme_runs, _) = observe(&fx, "settings.theme");
    let (size_runs, size_seen) = observe(&fx, "settings.size");
    let (count_runs, _) = observe(&fx, "count");

    fx.scope.invoke("grow", &js_sys::Array::new());
    flush_sync();

    assert_eq!(size_runs.get(), 2, "mutated leaf re-runs");
    assert_eq!(theme_runs.get(), 1, "untouched sibling stays quiet");
    assert_eq!(count_runs.get(), 1, "untouched root stays quiet");
    assert_eq!(size_seen.borrow().as_f64(), Some(15.0));

    fx.scope.invoke("retheme", &js_sys::Array::new());
    flush_sync();
    assert_eq!(theme_runs.get(), 2);
    assert_eq!(size_runs.get(), 2);
}

#[wasm_bindgen_test]
fn handler_no_net_change_leaf_triggers_nothing() {
    setup();
    let fx = fixture();
    let (theme_runs, _) = observe(&fx, "settings.theme");
    let (size_runs, _) = observe(&fx, "settings.size");

    fx.scope.invoke("touch_restore", &js_sys::Array::new());
    flush_sync();

    assert_eq!(size_runs.get(), 1, "mutate-and-restore fires nothing");
    assert_eq!(theme_runs.get(), 1);
}

#[wasm_bindgen_test]
fn no_path_access_reach_degrades_to_root_granularity() {
    setup();
    let fx = fixture();
    // `blob` has no set_path/path_fingerprint arms — reads stay
    // root-tracked, writes take the snapshot round-trip.
    let (x_runs, x_seen) = observe(&fx, "blob.x");
    let (y_runs, _) = observe(&fx, "blob.y");

    let ok = pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        Some(&fx.access),
        "blob.x",
        &JsValue::from_f64(42.0),
    );
    assert!(ok, "snapshot fallback still lands the write");
    flush_sync();

    assert_eq!(fx.state.borrow().blob.x, 42);
    assert_eq!(fx.state.borrow().blob.y, 2, "sibling survives round-trip");
    assert_eq!(x_runs.get(), 2);
    assert_eq!(y_runs.get(), 2, "root granularity: sibling re-runs too");
    assert_eq!(x_seen.borrow().as_f64(), Some(42.0));
}

#[wasm_bindgen_test]
fn flat_fields_unaffected() {
    setup();
    let fx = fixture();
    let (count_runs, count_seen) = observe(&fx, "count");

    fx.scope.invoke("bump_count", &js_sys::Array::new());
    flush_sync();
    assert_eq!(count_runs.get(), 2);
    assert_eq!(count_seen.borrow().as_f64(), Some(1.0));

    // A leaf write elsewhere leaves flat subscribers alone.
    pocopine_core::path::write_path_with(
        &JsValue::UNDEFINED,
        Some(&fx.access),
        "settings.theme",
        &JsValue::from_str("dark"),
    );
    flush_sync();
    assert_eq!(count_runs.get(), 2);
}
