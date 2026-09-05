//! Regression coverage for synchronous browser events re-entering the
//! component handler that caused them.

#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::Array;
use pocopine_core::{ComponentState, Scope, ScopeId, defer_component_callback};
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

struct CrossScopeState {
    outer: bool,
    target: Rc<Cell<Option<ScopeId>>>,
    order: Rc<RefCell<Vec<&'static str>>>,
}

impl ComponentState for CrossScopeState {
    fn get(&self, _key: &str) -> JsValue {
        JsValue::UNDEFINED
    }

    fn set(&mut self, _key: &str, _value: JsValue) {}

    fn keys(&self) -> &'static [&'static str] {
        &[]
    }

    fn invoke(&mut self, key: &str, _args: &Array) -> JsValue {
        if self.outer && key == "run" {
            self.order.borrow_mut().push("outer:start");
            let target = self.target.get().expect("nested scope id");
            pocopine_core::scope::invoke_handler(target, "run", &Array::new());
            self.order.borrow_mut().push("outer:end");
        } else if !self.outer && key == "run" {
            self.order.borrow_mut().push("inner:start");
            let order = Rc::clone(&self.order);
            defer_component_callback(move || order.borrow_mut().push("deferred"));
            self.order.borrow_mut().push("inner:end");
        }
        JsValue::UNDEFINED
    }
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

#[wasm_bindgen_test]
fn nested_cross_scope_handlers_share_one_outer_fifo_safe_point() {
    let order = Rc::new(RefCell::new(Vec::new()));
    let target = Rc::new(Cell::new(None));
    let outer = Scope::new(Rc::new(RefCell::new(CrossScopeState {
        outer: true,
        target: Rc::clone(&target),
        order: Rc::clone(&order),
    })));
    let inner = Scope::new(Rc::new(RefCell::new(CrossScopeState {
        outer: false,
        target: Rc::clone(&target),
        order: Rc::clone(&order),
    })));
    target.set(Some(inner.id));

    outer.invoke("run", &Array::new());

    assert_eq!(
        order.borrow().as_slice(),
        [
            "outer:start",
            "inner:start",
            "inner:end",
            "outer:end",
            "deferred",
        ],
        "the nested scope stays synchronous but cannot drain work through the outer scope",
    );
    Scope::remove(inner.id);
    Scope::remove(outer.id);
}

/// The trap's `focusin` listener pulls focus back into its container by
/// calling `focus()` — which dispatches the next `focusin` synchronously,
/// while the listener is still running. A `dyn FnMut` closure is refused
/// re-entry by wasm-bindgen ("closure invoked recursively or after being
/// dropped"): the nested dispatch became an uncaught error on `window`
/// every time a trap corrected focus.
#[wasm_bindgen_test]
fn focus_trap_correction_does_not_reenter_its_own_listener() {
    let window = web_sys::window().expect("browser window");
    let document = window.document().expect("browser document");
    let body = document.body().expect("document body");

    let container = document.create_element("div").expect("create container");
    let inside = document.create_element("button").expect("create button");
    container.append_child(&inside).expect("attach button");
    body.append_child(&container).expect("attach container");
    let outside = document.create_element("input").expect("create input");
    body.append_child(&outside).expect("attach input");

    let errors = Rc::new(Cell::new(0u32));
    let errors_seen = errors.clone();
    let on_error = wasm_bindgen::closure::Closure::<dyn Fn(web_sys::ErrorEvent)>::new(
        move |event: web_sys::ErrorEvent| {
            errors_seen.set(errors_seen.get() + 1);
            event.prevent_default();
        },
    );
    window
        .add_event_listener_with_callback("error", on_error.as_ref().unchecked_ref())
        .expect("listen for uncaught errors");

    let trap = pocopine_core::focus::trap(&container);
    outside
        .clone()
        .unchecked_into::<HtmlElement>()
        .focus()
        .expect("focus the input outside the trap");

    let active = document.active_element().expect("something is focused");
    let uncaught = errors.get();

    // Tear down before asserting: a trap left behind by a failed run would
    // hijack every later focus in this suite. Blur first — removing the
    // focused element leaves Firefox without a focus target, and the next
    // `focus()` in the suite then dispatches no event.
    trap.release();
    let _ = inside.clone().unchecked_into::<HtmlElement>().blur();
    window
        .remove_event_listener_with_callback("error", on_error.as_ref().unchecked_ref())
        .expect("stop listening");
    container.remove();
    outside.remove();

    assert_eq!(active, inside, "the trap pulled focus back inside");
    assert_eq!(
        uncaught, 0,
        "the corrective focus re-entered the trap's focusin listener",
    );
}
