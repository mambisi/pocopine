//! RFC 056 §6.8 — `#[derive(Emit)]` exercise.
//!
//! Pins the variant→event-name mapping the derive emits and the
//! `to_detail` payload shape (unit, struct, tuple). Lives in the
//! umbrella crate so the derive's `::pocopine::__private::*`
//! references resolve.
//!
//! Run with `wasm-pack test --node crates/pocopine`.

#![cfg(target_arch = "wasm32")]

use pocopine::Emit as EmitTrait;
use pocopine_macros::Emit;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

#[derive(Emit)]
enum DialogEvent {
    Close,
    OpenChange,
    Confirm { value: String, count: u32 },
    Pair(u32, u32),
}

#[wasm_bindgen_test]
fn unit_variant_kebabs() {
    assert_eq!(DialogEvent::Close.event_name(), "close");
}

#[wasm_bindgen_test]
fn camel_case_variant_kebabs() {
    assert_eq!(DialogEvent::OpenChange.event_name(), "open-change");
}

#[wasm_bindgen_test]
fn struct_variant_kebabs_and_serialises_named_payload() {
    let ev = DialogEvent::Confirm {
        value: "ok".into(),
        count: 3,
    };
    assert_eq!(ev.event_name(), "confirm");
    let detail: JsValue = ev.to_detail().expect("serialise");
    let value: String =
        serde_wasm_bindgen::from_value(js_sys::Reflect::get(&detail, &"value".into()).unwrap())
            .unwrap();
    let count: u32 =
        serde_wasm_bindgen::from_value(js_sys::Reflect::get(&detail, &"count".into()).unwrap())
            .unwrap();
    assert_eq!(value, "ok");
    assert_eq!(count, 3);
}

#[wasm_bindgen_test]
fn tuple_variant_serialises_positional_payload() {
    let ev = DialogEvent::Pair(7, 11);
    assert_eq!(ev.event_name(), "pair");
    let detail: JsValue = ev.to_detail().expect("serialise");
    let arr: js_sys::Array = detail.dyn_into().expect("array");
    assert_eq!(arr.length(), 2);
    assert_eq!(arr.get(0).as_f64().unwrap() as u32, 7);
    assert_eq!(arr.get(1).as_f64().unwrap() as u32, 11);
}
