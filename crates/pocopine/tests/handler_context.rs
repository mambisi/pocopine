//! Runtime coverage for the explicit `#[context]` handler parameter lane.

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Reflect};
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "handler-context-probe",
    template = poco! { <button>probe</button> }
)]
struct HandlerContextProbe {
    before: String,
    after: String,
    failure_handler_ran: bool,
}

struct MissingCapability;

impl FromHandlerContext for MissingCapability {
    fn from_handler_context(context: &HandlerContext) -> Result<Self, HandlerExtractError> {
        Err(context.extraction_error("the requested test capability is not installed"))
    }
}

#[handlers]
impl HandlerContextProbe {
    pub fn context_before(
        &mut self,
        #[context] context: HandlerContext,
        label: String,
        count: f64,
    ) {
        self.before = format!(
            "{label}:{count}:{}:{}:{}",
            context.component(),
            context.handler(),
            context.scope_id().expect("scope is active").0,
        );
    }

    pub fn context_after(&mut self, label: String, count: f64, #[context] context: HandlerContext) {
        self.after = format!(
            "{label}:{count}:{}:{}:{}",
            context.component(),
            context.handler(),
            context.scope_id().expect("scope is active").0,
        );
    }

    pub fn extraction_failure(&mut self, #[context] _missing: MissingCapability) {
        self.failure_handler_ran = true;
    }
}

fn event_args(label: &str, count: f64) -> Array {
    let args = Array::new();
    args.push(&JsValue::from_str(label));
    args.push(&JsValue::from_f64(count));
    args
}

fn diagnostic_string(value: &JsValue, key: &str) -> String {
    Reflect::get(value, &JsValue::from_str(key))
        .expect("diagnostic property")
        .as_string()
        .expect("string diagnostic property")
}

#[wasm_bindgen_test]
fn context_parameters_do_not_consume_event_argument_slots() {
    let state = Rc::new(RefCell::new(HandlerContextProbe::default()));
    let scope = Scope::new(Rc::clone(&state));

    scope.invoke("context_before", &event_args("first", 1.0));
    scope.invoke("context_after", &event_args("last", 2.0));

    let state = state.borrow();
    assert!(
        state.before.starts_with("first:1:"),
        "context before ordinary arguments must leave slot zero untouched",
    );
    assert!(state.before.contains("HandlerContextProbe:context_before:"));
    assert!(
        state.after.starts_with("last:2:"),
        "context after ordinary arguments must not request a third JS slot",
    );
    assert!(state.after.contains("HandlerContextProbe:context_after:"));
    drop(state);

    Scope::remove(scope.id);
}

#[wasm_bindgen_test]
fn extraction_failure_returns_a_structured_diagnostic_and_skips_the_handler() {
    let state = Rc::new(RefCell::new(HandlerContextProbe::default()));
    let scope = Scope::new(Rc::clone(&state));

    let diagnostic = scope.invoke("extraction_failure", &Array::new());

    assert!(!state.borrow().failure_handler_ran);
    assert_eq!(
        diagnostic_string(&diagnostic, "kind"),
        "pocopine.handler_context_extraction_failed",
    );
    assert!(diagnostic_string(&diagnostic, "component").contains("HandlerContextProbe"));
    assert_eq!(
        diagnostic_string(&diagnostic, "handler"),
        "extraction_failure",
    );
    assert_eq!(diagnostic_string(&diagnostic, "parameter"), "_missing");
    assert!(diagnostic_string(&diagnostic, "requested_type").contains("MissingCapability"));
    assert_eq!(
        diagnostic_string(&diagnostic, "reason"),
        "the requested test capability is not installed",
    );
    assert_eq!(
        Reflect::get(&diagnostic, &JsValue::from_str("scope_id"))
            .expect("scope id")
            .as_f64(),
        Some(scope.id.0 as f64),
    );

    Scope::remove(scope.id);
}
