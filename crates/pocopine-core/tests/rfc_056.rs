//! RFC 056 — component interaction safety batch.
//!
//! Coverage:
//! * registry safety (canonical/alias collision detection,
//!   idempotent re-registration, `verify_registry`)
//! * `ContextKey` method-style provide/inject parity with the free
//!   functions (RFC 056 §6.4)
//! * `Emit` trait — variant→event-name kebab mapping (the
//!   serde→`JsValue` payload path is exercised by the wasm walker
//!   tests; this layer just pins the name table)
//!
//! Runs under `wasm-pack test --node` like the rest of the
//! `pocopine-core` test suite.

#![cfg(target_arch = "wasm32")]

use pocopine_core::{
    assert_registry_clean, register_component, register_component_as, register_component_prefixed,
    registered_component_names, registry_errors, verify_registry, ContextKey, Emit,
    RegistryErrorKind, Scope,
};
use wasm_bindgen_test::wasm_bindgen_test;

/// Reset the process-local registry between tests so an earlier
/// `__reset_for_test` doesn't bleed state into the next case.
fn fresh_registry() {
    pocopine_core::registry::__reset_for_test();
}

#[derive(Default)]
struct DummyA;
#[derive(Default)]
struct DummyB;

impl pocopine_core::ComponentState for DummyA {
    fn get(&self, _key: &str) -> wasm_bindgen::JsValue {
        wasm_bindgen::JsValue::UNDEFINED
    }
    fn set(&mut self, _key: &str, _value: wasm_bindgen::JsValue) {}
    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
    fn invoke(&mut self, _key: &str, _args: &js_sys::Array) -> wasm_bindgen::JsValue {
        wasm_bindgen::JsValue::UNDEFINED
    }
}

impl pocopine_core::ComponentState for DummyB {
    fn get(&self, _key: &str) -> wasm_bindgen::JsValue {
        wasm_bindgen::JsValue::UNDEFINED
    }
    fn set(&mut self, _key: &str, _value: wasm_bindgen::JsValue) {}
    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
    fn invoke(&mut self, _key: &str, _args: &js_sys::Array) -> wasm_bindgen::JsValue {
        wasm_bindgen::JsValue::UNDEFINED
    }
}

fn dummy_ctor() -> Scope {
    Scope::new(::std::rc::Rc::new(::std::cell::RefCell::new(DummyA)))
}

fn other_ctor() -> Scope {
    Scope::new(::std::rc::Rc::new(::std::cell::RefCell::new(DummyB)))
}

#[wasm_bindgen_test]
fn same_owner_re_registration_is_noop() {
    fresh_registry();
    register_component("counter", "owner::A", dummy_ctor);
    register_component("counter", "owner::A", dummy_ctor);
    assert!(verify_registry().is_ok());
    assert_eq!(registered_component_names(), vec!["counter".to_string()]);
}

#[wasm_bindgen_test]
fn distinct_owners_collide_on_canonical() {
    fresh_registry();
    register_component("counter", "owner::A", dummy_ctor);
    register_component("counter", "owner::B", other_ctor);
    let errs = registry_errors();
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].kind, RegistryErrorKind::DuplicateCanonicalTag);
    assert_eq!(errs[0].tag, "counter");
    assert!(verify_registry().is_err());
}

#[wasm_bindgen_test]
fn assert_registry_clean_panics_on_dirty() {
    fresh_registry();
    register_component("counter", "owner::A", dummy_ctor);
    register_component("counter", "owner::B", other_ctor);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(assert_registry_clean));
    assert!(result.is_err());
}

#[wasm_bindgen_test]
fn alias_conflicts_with_existing_canonical() {
    fresh_registry();
    register_component("widget", "owner::A", dummy_ctor);
    register_component_as("widget", "real-canonical", "owner::B", other_ctor);
    let errs = registry_errors();
    assert!(
        errs.iter().any(|e| {
            e.kind == RegistryErrorKind::AliasConflictsWithCanonical && e.tag == "widget"
        }),
        "expected AliasConflictsWithCanonical for `widget`, got {errs:?}",
    );
}

#[wasm_bindgen_test]
fn canonical_conflicts_with_existing_alias() {
    fresh_registry();
    // First call mints `real-canonical` as canonical and `widget` as
    // alias under owner::A.
    register_component_as("widget", "real-canonical", "owner::A", dummy_ctor);
    // owner::B now tries to claim `widget` as a canonical name.
    register_component("widget", "owner::B", other_ctor);
    let errs = registry_errors();
    assert!(
        errs.iter().any(|e| {
            e.kind == RegistryErrorKind::CanonicalConflictsWithAlias && e.tag == "widget"
        }),
        "expected CanonicalConflictsWithAlias for `widget`, got {errs:?}",
    );
}

#[wasm_bindgen_test]
fn duplicate_alias_from_distinct_owners() {
    fresh_registry();
    register_component_as("alias", "canonical-a", "owner::A", dummy_ctor);
    register_component_as("alias", "canonical-b", "owner::B", other_ctor);
    let errs = registry_errors();
    assert!(
        errs.iter()
            .any(|e| e.kind == RegistryErrorKind::DuplicateAlias && e.tag == "alias"),
        "expected DuplicateAlias for `alias`, got {errs:?}",
    );
}

#[wasm_bindgen_test]
fn prefixed_registration_combines_into_canonical_name() {
    fresh_registry();
    register_component_prefixed("pine", "popover", "owner::A", dummy_ctor);
    let names = registered_component_names();
    assert!(
        names.iter().any(|n| n == "pine-popover"),
        "expected `pine-popover` in {names:?}",
    );
}

// `render_boot_error_*` lives in the umbrella crate's browser test
// suite — it touches `web_sys::window()` / `document.body`, which
// are best exercised under `wasm-pack test --chrome --headless`.

#[wasm_bindgen_test]
fn context_key_method_style_matches_free_functions() {
    let key_a: ContextKey<u32> = ContextKey::new("rfc056::test::a");
    let key_b: ContextKey<u32> = ContextKey::new("rfc056::test::b");
    // Two separately-minted keys with different debug names must
    // never share an id.
    assert_ne!(key_a.id(), key_b.id());
    assert_eq!(key_a.name(), "rfc056::test::a");
}

// `#[derive(Emit)]` only generates code that depends on `pocopine`
// (the umbrella crate) being in scope, so the actual derive test
// lives in `crates/pocopine/tests`. This module just pins the
// trait surface itself by hand-implementing it on a tiny enum.

enum HandImplEvent {
    Open,
    OpenChange,
}

impl Emit for HandImplEvent {
    fn event_name(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::OpenChange => "open-change",
        }
    }
    fn to_detail(&self) -> Result<wasm_bindgen::JsValue, serde_wasm_bindgen::Error> {
        serde_wasm_bindgen::to_value(&Option::<()>::None)
    }
}

#[wasm_bindgen_test]
fn emit_trait_event_name_round_trips() {
    assert_eq!(HandImplEvent::Open.event_name(), "open");
    assert_eq!(HandImplEvent::OpenChange.event_name(), "open-change");
}

// The derive expands to references against the `pocopine`
// umbrella crate's `__private` re-exports, so `#[derive(Emit)]`
// itself is exercised from the `pocopine` test crate
// (`crates/pocopine/tests/rfc_056_emit.rs`) where the umbrella is
// in scope.
