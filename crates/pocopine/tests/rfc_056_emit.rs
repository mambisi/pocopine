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

#[wasm_bindgen_test]
fn event_names_lists_every_variant_kebab() {
    assert_eq!(
        <DialogEvent as EmitTrait>::event_names(),
        &["close", "open-change", "confirm", "pair"],
    );
}

#[wasm_bindgen_test]
fn from_event_round_trips_unit_variant() {
    let recovered = <DialogEvent as EmitTrait>::from_event("close", JsValue::UNDEFINED);
    assert!(matches!(recovered, Some(DialogEvent::Close)));
    let recovered2 = <DialogEvent as EmitTrait>::from_event("open-change", JsValue::UNDEFINED);
    assert!(matches!(recovered2, Some(DialogEvent::OpenChange)));
}

#[wasm_bindgen_test]
fn from_event_round_trips_struct_variant() {
    let original = DialogEvent::Confirm {
        value: "ok".into(),
        count: 3,
    };
    let detail = original.to_detail().expect("serialise");
    let recovered = <DialogEvent as EmitTrait>::from_event("confirm", detail).expect("recover");
    match recovered {
        DialogEvent::Confirm { value, count } => {
            assert_eq!(value, "ok");
            assert_eq!(count, 3);
        }
        _ => panic!("wrong variant"),
    }
}

#[wasm_bindgen_test]
fn from_event_round_trips_tuple_variant() {
    let original = DialogEvent::Pair(7, 11);
    let detail = original.to_detail().expect("serialise");
    let recovered = <DialogEvent as EmitTrait>::from_event("pair", detail).expect("recover");
    assert!(matches!(recovered, DialogEvent::Pair(7, 11)));
}

#[wasm_bindgen_test]
fn from_event_unknown_name_yields_none() {
    let recovered = <DialogEvent as EmitTrait>::from_event("does-not-exist", JsValue::UNDEFINED);
    assert!(recovered.is_none());
}
