//! Regression coverage for synchronous browser events re-entering the
//! component handler that caused them.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Array;
use pocopine_core::{ComponentState, Scope};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::HtmlElement;

wasm_bindgen_test_configure!(run_in_browser);

struct FocusState {
    input: HtmlElement,
    order: Rc<RefCell<Vec<&'static str>>>,
    focused: bool,
    assigned: bool,
}

impl ComponentState for FocusState {
    fn get(&self, key: &str) -> JsValue {
        match key {
            "focused" => self.focused.into(),
            "assigned" => self.assigned.into(),
            _ => JsValue::UNDEFINED,
        }
    }

    fn set(&mut self, key: &str, value: JsValue) {
        if key == "assigned" {
            self.assigned = value.as_bool().unwrap_or(false);
        }
    }

    fn keys(&self) -> &'static [&'static str] {
        &["focused", "assigned"]
    }

    fn invoke(&mut self, key: &str, _args: &Array) -> JsValue {
        match key {
            "open" => {
                self.order.borrow_mut().push("outer:start");
                self.input.focus().expect("input focuses");
                self.order.borrow_mut().push("outer:end");
            }
            "on_focus" => {
                self.order.borrow_mut().push("focus");
                self.focused = true;
            }
            _ => {}
        }
        JsValue::UNDEFINED
    }
}

#[wasm_bindgen_test]
fn focus_event_waits_for_the_active_handler_borrow_to_end() {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .expect("browser document");
    let element = document.create_element("input").expect("create input");
    let input = element.clone().unchecked_into::<HtmlElement>();
    document
        .body()
        .expect("document body")
        .append_child(&element)
        .expect("attach input");

    let order = Rc::new(RefCell::new(Vec::new()));
    let state = Rc::new(RefCell::new(FocusState {
        input,
        order: order.clone(),
        focused: false,
        assigned: false,
    }));
    let scope = Scope::new(state.clone());
    let proxy = scope.into_proxy();
    let ast = Rc::new(
        pocopine_core::expr::parse_cached("on_focus($event); assigned = true")
            .expect("event expression parses"),
    );
    pocopine_core::directives::on::install(&element, scope.id, &proxy, "focus", &[], Some(ast));

    // `HtmlElement::focus()` dispatches `focus` before it returns. The event
    // expression includes both a handler call and a state assignment so the
    // complete expression—not just the call—must wait for `open` to release
    // the component's mutable borrow.
    scope.invoke("open", &Array::new());

    assert_eq!(
        order.borrow().as_slice(),
        ["outer:start", "outer:end", "focus"],
        "focus work drains in the same turn, after the initiating handler",
    );
    assert!(state.borrow().focused, "focus handler ran");
    assert!(
        state.borrow().assigned,
        "the rest of the event expression ran without re-borrowing state",
    );

    Scope::remove(scope.id);
    element.remove();
}
