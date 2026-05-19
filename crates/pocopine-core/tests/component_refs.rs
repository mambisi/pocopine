//! RFC 081 — `refs::get_component` (parent reaches a typed
//! `Handle<Child>` via the same `pp-ref="name"` directive used
//! for DOM element refs).
//!
//! Tests exercise the resolution layer directly: manually build
//! a child scope, stamp the simulated host element with the
//! child's `SCOPE_ID_KEY` (mirrors what
//! [`crate::mount::mount_component`] now does), register the
//! host under a parent scope's ref table, and call
//! [`crate::refs::get_component_on`].
//!
//! The full template-walker round-trip (parent's `.poco`
//! mounting `<test-child pp-ref="body">`) is covered by the
//! Phase 3 integration suite once a worked example lands.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Array;
use pocopine_core::{refs, ComponentState, Scope};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

// These tests construct real `Element`s (`document.create_element`)
// to simulate a child-component host with `SCOPE_ID_KEY` stamped on
// it. Node has no DOM, so route the suite through the browser
// runner via `wasm-pack test --firefox --headless` (matches
// `svg_namespace.rs`).
wasm_bindgen_test_configure!(run_in_browser);

/// Minimal stand-in for a component's state. Lets the tests
/// create real scopes via [`Scope::new`] without dragging in
/// the `#[component]` macro.
#[derive(Default)]
struct ChildComponent {
    label: String,
}

impl ComponentState for ChildComponent {
    fn get(&self, _key: &str) -> JsValue {
        JsValue::UNDEFINED
    }
    fn set(&mut self, _key: &str, _value: JsValue) {}
    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

#[derive(Default)]
struct ParentComponent;

impl ComponentState for ParentComponent {
    fn get(&self, _key: &str) -> JsValue {
        JsValue::UNDEFINED
    }
    fn set(&mut self, _key: &str, _value: JsValue) {}
    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

#[derive(Default)]
struct UnrelatedType;

impl ComponentState for UnrelatedType {
    fn get(&self, _key: &str) -> JsValue {
        JsValue::UNDEFINED
    }
    fn set(&mut self, _key: &str, _value: JsValue) {}
    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
    fn invoke(&mut self, _key: &str, _args: &Array) -> JsValue {
        JsValue::UNDEFINED
    }
}

fn make_host(scope_id_to_stamp: Option<pocopine_core::reactive::ScopeId>) -> web_sys::Element {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .expect("test runs in wasm-pack node with document");
    let el = doc.create_element("div").expect("create_element succeeds");
    if let Some(scope_id) = scope_id_to_stamp {
        pocopine_core::mount::bind_scope_id_only(&el, scope_id);
    }
    el
}

#[wasm_bindgen_test]
fn get_component_returns_typed_handle_for_named_child() {
    let child_state = Rc::new(RefCell::new(ChildComponent {
        label: "hello".into(),
    }));
    let child_scope = Scope::new(child_state);
    let parent_state = Rc::new(RefCell::new(ParentComponent));
    let parent_scope = Scope::new(parent_state);

    let host = make_host(Some(child_scope.id));
    refs::register(parent_scope.id, "body", &host);

    let handle =
        refs::get_component_on::<ChildComponent>(parent_scope.id, "body").expect("handle resolves");
    let observed = handle.with(|c| c.label.clone());
    assert_eq!(observed, "hello", "handle reads child state");
}

#[wasm_bindgen_test]
fn get_component_returns_none_for_mismatched_type() {
    let child_state = Rc::new(RefCell::new(ChildComponent::default()));
    let child_scope = Scope::new(child_state);
    let parent_state = Rc::new(RefCell::new(ParentComponent));
    let parent_scope = Scope::new(parent_state);

    let host = make_host(Some(child_scope.id));
    refs::register(parent_scope.id, "body", &host);

    let wrong = refs::get_component_on::<UnrelatedType>(parent_scope.id, "body");
    assert!(
        wrong.is_none(),
        "wrong type at the registered scope must miss, not coerce"
    );
}

#[wasm_bindgen_test]
fn get_component_returns_none_for_plain_dom_ref() {
    // pp-ref on a plain element (not a child component host) —
    // no SCOPE_ID_KEY stamped. Lookup must miss cleanly.
    let parent_state = Rc::new(RefCell::new(ParentComponent));
    let parent_scope = Scope::new(parent_state);

    let host = make_host(None);
    refs::register(parent_scope.id, "title_input", &host);

    let missed = refs::get_component_on::<ChildComponent>(parent_scope.id, "title_input");
    assert!(
        missed.is_none(),
        "plain DOM ref must not resolve as a child"
    );
}

#[wasm_bindgen_test]
fn get_component_returns_none_for_unknown_ref() {
    let parent_state = Rc::new(RefCell::new(ParentComponent));
    let parent_scope = Scope::new(parent_state);

    let missed = refs::get_component_on::<ChildComponent>(parent_scope.id, "never_registered");
    assert!(missed.is_none(), "unknown ref name returns None");
}

#[wasm_bindgen_test]
fn get_component_rejects_self_scope() {
    // Same-scope element (the parent's own template root)
    // would also carry SCOPE_ID_KEY. The lookup guards
    // against returning "self as child."
    let parent_state = Rc::new(RefCell::new(ParentComponent));
    let parent_scope = Scope::new(parent_state);

    let host = make_host(Some(parent_scope.id));
    refs::register(parent_scope.id, "self_ref", &host);

    let missed = refs::get_component_on::<ParentComponent>(parent_scope.id, "self_ref");
    assert!(
        missed.is_none(),
        "ref pointing to the same scope must not be treated as a child"
    );
}

#[wasm_bindgen_test]
fn get_component_evicts_with_clear_scope() {
    let child_state = Rc::new(RefCell::new(ChildComponent::default()));
    let child_scope = Scope::new(child_state);
    let parent_state = Rc::new(RefCell::new(ParentComponent));
    let parent_scope = Scope::new(parent_state);

    let host = make_host(Some(child_scope.id));
    refs::register(parent_scope.id, "body", &host);
    assert!(refs::get_component_on::<ChildComponent>(parent_scope.id, "body").is_some());

    refs::clear_scope(parent_scope.id);

    let after = refs::get_component_on::<ChildComponent>(parent_scope.id, "body");
    assert!(
        after.is_none(),
        "clear_scope must also drop the component-ref lookup"
    );
}
