//! Runs under `wasm-pack test --firefox --headless crates/pocopine-core --test client_module`.

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Function, Object, Promise, Reflect};
use pocopine_core::{ClientModule, ScopeId};
use serde::Deserialize;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[derive(Debug, Deserialize, PartialEq)]
struct Payload {
    message: String,
}

#[wasm_bindgen_test(async)]
async fn call_async_deserializes_client_module_payload() {
    let module = Object::new();
    let load = Function::new_no_args("return Promise.resolve({ message: 'ready' });");
    Reflect::set(&module, &"load".into(), load.as_ref()).unwrap();
    register_module("demo", module.as_ref());

    let result: Payload = ClientModule::required("demo")
        .unwrap()
        .call_async("load")
        .await
        .unwrap();

    assert_eq!(
        result,
        Payload {
            message: "ready".to_string()
        }
    );
}

#[wasm_bindgen_test(async)]
async fn subscribe_deserializes_callback_payload_after_the_setup_stack() {
    let module = Object::new();
    let on_state = Function::new_with_args(
        "callback",
        "try { callback({ message: 'signed-in' }); } catch (error) { globalThis.__pp_client_module_test_error = error && (error.stack || error.message) || String(error); } return function unsubscribe() {};",
    );
    Reflect::set(&module, &"onState".into(), on_state.as_ref()).unwrap();
    register_module("auth", module.as_ref());

    let seen = Rc::new(RefCell::new(Vec::new()));
    let seen_for_handler = seen.clone();
    ClientModule::required("auth")
        .unwrap()
        .subscribe(
            ScopeId(u64::MAX),
            "onState",
            move |result: Result<Payload, _>| {
                let message = result
                    .map(|payload| payload.message)
                    .unwrap_or_else(|err| format!("error: {err}"));
                seen_for_handler.borrow_mut().push(message);
            },
        )
        .unwrap();

    assert!(seen.borrow().is_empty());
    next_microtask().await;

    let js_error =
        Reflect::get(&js_sys::global(), &"__pp_client_module_test_error".into()).unwrap();
    assert!(
        js_error.is_undefined(),
        "callback threw: {:?}",
        js_error.as_string()
    );
    assert_eq!(seen.borrow().as_slice(), ["signed-in"]);
}

#[wasm_bindgen_test]
fn required_reports_missing_module() {
    register_empty_registry();

    let err = ClientModule::required("missing").unwrap_err();

    assert_eq!(err.message(), "client module `missing` is not registered");
}

fn register_module(name: &str, module: &JsValue) {
    let registry = Object::new();
    Reflect::set(&registry, &JsValue::from_str(name), module).unwrap();
    Reflect::set(
        &js_sys::global(),
        &"__pp_client_modules".into(),
        registry.as_ref(),
    )
    .unwrap();
}

fn register_empty_registry() {
    let registry = Object::new();
    Reflect::set(
        &js_sys::global(),
        &"__pp_client_modules".into(),
        registry.as_ref(),
    )
    .unwrap();
}

async fn next_microtask() {
    JsFuture::from(Promise::resolve(&JsValue::NULL))
        .await
        .unwrap();
}
