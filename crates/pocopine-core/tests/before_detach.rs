#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use pocopine_core::{ComponentState, Scope, mount, on_before_detach};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

struct State(Rc<RefCell<Vec<&'static str>>>);

impl ComponentState for State {
    fn get(&self, _: &str) -> JsValue {
        JsValue::UNDEFINED
    }
    fn set(&mut self, _: &str, _: JsValue) {}
    fn keys(&self) -> &'static [&'static str] {
        &[]
    }
    fn invoke(&mut self, _: &str, _: &js_sys::Array) -> JsValue {
        JsValue::UNDEFINED
    }
    fn unmount(&mut self, _: pocopine_core::lifecycle::LifecycleContext<'_>) {
        self.0.borrow_mut().push("unmount");
    }
}

fn element() -> web_sys::Element {
    web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .create_element("div")
        .unwrap()
}

#[wasm_bindgen_test]
fn all_finalizers_run_attached_before_any_descendant_scope_is_released() {
    let events = Rc::new(RefCell::new(Vec::new()));
    let root = element();
    let child = element();
    root.append_child(&child).unwrap();
    web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .body()
        .unwrap()
        .append_child(&root)
        .unwrap();
    let scope = Scope::new(Rc::new(RefCell::new(State(events.clone()))));
    mount::bind_scope_id_only(&child, scope.id);
    for owner in [&root, &child] {
        let el = owner.clone();
        let events = events.clone();
        let id = scope.id;
        on_before_detach(owner, move || {
            assert!(el.is_connected());
            assert!(Scope::find(id).is_some());
            assert!(!events.borrow().contains(&"unmount"));
            events.borrow_mut().push("finalize");
        });
    }
    mount::release_compiled_subtree(&root);
    root.remove();
    assert_eq!(&*events.borrow(), &["finalize", "finalize", "unmount"]);
    mount::release_compiled_subtree(&root);
    assert_eq!(events.borrow().len(), 3);
}

#[wasm_bindgen_test]
fn unrelated_editors_stay_live_and_external_detach_is_observable() {
    let first = element();
    let second = element();
    let events = Rc::new(RefCell::new(Vec::new()));
    let first_events = events.clone();
    on_before_detach(&first, move || first_events.borrow_mut().push("first"));
    let second_events = events.clone();
    let second_el = second.clone();
    on_before_detach(&second, move || {
        assert!(!second_el.is_connected());
        second_events.borrow_mut().push("second detached");
    });
    mount::release_compiled_subtree(&first);
    assert_eq!(&*events.borrow(), &["first"]);
    mount::release_compiled_subtree(&second);
    assert_eq!(&*events.borrow(), &["first", "second detached"]);
}

#[wasm_bindgen_test]
fn teleported_finalizer_runs_before_its_logical_owner_releases() {
    let owner = element();
    let origin = element();
    owner.append_child(&origin).unwrap();
    let portal = element();
    let body = web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .body()
        .unwrap();
    body.append_child(&owner).unwrap();
    body.append_child(&portal).unwrap();
    js_sys::Reflect::set(
        &portal,
        &pocopine_core::directives::teleport::TELEPORT_ORIGIN_KEY.into(),
        &origin,
    )
    .unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = events.clone();
    let portal_el = portal.clone();
    on_before_detach(&portal, move || {
        assert!(portal_el.is_connected());
        observed.borrow_mut().push("portal");
    });
    mount::release_compiled_subtree(&owner);
    assert_eq!(&*events.borrow(), &["portal"]);
    owner.remove();
    portal.remove();
    mount::release_compiled_subtree(&portal);
    assert_eq!(events.borrow().len(), 1);
}
