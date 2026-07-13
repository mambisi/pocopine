//! Focused safe-point coverage for typed component updates.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Array;
use pocopine_core::{ComponentState, Handle, Scope, defer_component_callback};
use wasm_bindgen::JsValue;

#[derive(Default)]
struct CallbackState {
    value: usize,
}

impl ComponentState for CallbackState {
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

#[test]
fn handle_update_releases_its_self_borrow_before_deferred_work() {
    let state = Rc::new(RefCell::new(CallbackState::default()));
    let scope = Scope::new(Rc::clone(&state));
    let handle = Handle::new(Rc::clone(&state), scope.id);
    let observed = Rc::new(RefCell::new(Vec::new()));

    handle.update(|component| {
        component.value = 1;
        observed.borrow_mut().push("update:start");
        let state_for_deferred = Rc::clone(&state);
        let observed_for_deferred = Rc::clone(&observed);
        defer_component_callback(move || {
            let mut component = state_for_deferred
                .try_borrow_mut()
                .expect("Handle::update released its RefMut before draining");
            component.value += 1;
            observed_for_deferred.borrow_mut().push("deferred");
        });
        observed.borrow_mut().push("update:end");
        assert_eq!(component.value, 1);
    });

    assert_eq!(
        *observed.borrow(),
        ["update:start", "update:end", "deferred"]
    );
    assert_eq!(state.borrow().value, 2);
    Scope::remove(scope.id);
}

#[test]
fn nested_cross_scope_updates_share_the_outer_safe_point() {
    let first = Rc::new(RefCell::new(CallbackState::default()));
    let second = Rc::new(RefCell::new(CallbackState::default()));
    let first_scope = Scope::new(Rc::clone(&first));
    let second_scope = Scope::new(Rc::clone(&second));
    let first_handle = Handle::new(Rc::clone(&first), first_scope.id);
    let second_handle = Handle::new(Rc::clone(&second), second_scope.id);
    let order = Rc::new(RefCell::new(Vec::new()));

    first_handle.update(|first_state| {
        order.borrow_mut().push("first:start");
        first_state.value = 1;
        second_handle.update(|second_state| {
            order.borrow_mut().push("second");
            second_state.value = 2;
            let order_for_deferred = Rc::clone(&order);
            let first_for_deferred = Rc::clone(&first);
            let second_for_deferred = Rc::clone(&second);
            defer_component_callback(move || {
                assert!(first_for_deferred.try_borrow_mut().is_ok());
                assert!(second_for_deferred.try_borrow_mut().is_ok());
                order_for_deferred.borrow_mut().push("deferred");
            });
        });
        assert_eq!(
            order.borrow().as_slice(),
            ["first:start", "second"],
            "dropping the nested frame must not drain while its parent is active",
        );
        order.borrow_mut().push("first:end");
    });

    assert_eq!(
        *order.borrow(),
        ["first:start", "second", "first:end", "deferred"],
    );
    Scope::remove(first_scope.id);
    Scope::remove(second_scope.id);
}

#[test]
fn deferred_handle_update_waits_for_the_active_component_borrow() {
    let state = Rc::new(RefCell::new(CallbackState::default()));
    let scope = Scope::new(Rc::clone(&state));
    let handle = Handle::new(Rc::clone(&state), scope.id);

    {
        let _frame = pocopine_core::ComponentCallbackFrame::for_scope(scope.id);
        let active_borrow = state.borrow();
        handle.defer_update(|component| component.value = 7);
        assert_eq!(
            active_borrow.value, 0,
            "mutation must wait for the safe point"
        );
    }

    assert_eq!(state.borrow().value, 7);
    Scope::remove(scope.id);
}
