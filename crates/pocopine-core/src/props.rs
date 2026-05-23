//! RFC-044 §5.10 — `Props` and `PropValue` traits backing
//! `#[derive(Props)]`.
//!
//! A `Props` struct describes a reusable group of component props.
//! The `#[derive(Props)]` macro reads fields marked `#[prop]` on the
//! struct and emits an `impl Props` that exposes the leaf names and a
//! direct per-leaf `JsValue` conversion. When such a struct is
//! flattened into a `#[component]` field via a *bare*
//! `#[prop(flatten)]`, the `#[component]` macro routes leaf
//! get/set/static_prop_kind through this trait at runtime — avoiding
//! the whole-struct serde round-trip and providing exact per-leaf
//! types (no probe-the-default heuristic).

use wasm_bindgen::JsValue;

use crate::scope::StaticPropKind;

/// Conversion contract for an individual flatten-leaf value.
///
/// Every `#[prop]` field on a `#[derive(Props)]` struct must have a
/// type that implements `PropValue` — the derive uses these methods
/// to convert the field to/from a `JsValue` and to report the leaf's
/// `StaticPropKind` for HTML-attribute coercion.
///
/// v1 ships impls for `String`, `bool`, the common integer + float
/// types, and `Option<T: PropValue>`. Custom leaf types are future
/// work (an `#[prop(with = ...)]` escape hatch, RFC-044 future-work).
pub trait PropValue: Sized {
    /// Render this value as a `JsValue` for `prop_get`.
    fn to_prop_js(&self) -> JsValue;

    /// Parse a `JsValue` (typically the result of a parent's static
    /// attribute coercion or `pp-bind` write) into the leaf type.
    /// Returns `None` if the value cannot be converted; the
    /// generated `prop_set` arm drops the write silently in that
    /// case, matching the explicit-list flatten path's behaviour.
    fn from_prop_js(value: JsValue) -> Option<Self>;

    /// Static-attribute coercion hint for this leaf type.
    fn prop_static_kind() -> StaticPropKind;
}

impl PropValue for String {
    fn to_prop_js(&self) -> JsValue {
        JsValue::from_str(self)
    }
    fn from_prop_js(value: JsValue) -> Option<Self> {
        value.as_string()
    }
    fn prop_static_kind() -> StaticPropKind {
        StaticPropKind::String
    }
}

impl PropValue for bool {
    fn to_prop_js(&self) -> JsValue {
        JsValue::from_bool(*self)
    }
    fn from_prop_js(value: JsValue) -> Option<Self> {
        value.as_bool()
    }
    fn prop_static_kind() -> StaticPropKind {
        StaticPropKind::Bool
    }
}

macro_rules! impl_numeric_prop_value {
    ($($t:ty),* $(,)?) => { $(
        impl PropValue for $t {
            fn to_prop_js(&self) -> JsValue {
                JsValue::from_f64(*self as f64)
            }
            fn from_prop_js(value: JsValue) -> Option<Self> {
                value.as_f64().map(|n| n as $t)
            }
            fn prop_static_kind() -> StaticPropKind {
                StaticPropKind::Number
            }
        }
    )* };
}

// JS numbers are IEEE-754 doubles, so `i64` / `u64` over the safe-int
// range (±2^53) would silently lose precision through `as f64` and
// the round-trip back. Restrict the v1 impls to widths that round-trip
// losslessly. `usize` / `isize` are safe — pocopine targets wasm32
// only, where they are 32-bit. A future revision can add `i64` / `u64`
// via `BigInt` if a real prop type needs them.
impl_numeric_prop_value!(f32, f64, i8, i16, i32, isize, u8, u16, u32, usize);

/// `Option<T>` impl — `None` round-trips through `JsValue::NULL`, and
/// an empty-string inbound value coerces to `None` to match the
/// long-standing empty-attribute → `None` convention the explicit
/// flatten path also honours (RFC-044 §5.4).
///
/// Note this is lossy for `Option<String>` going through `pp-bind`:
/// a parent's `Some(String::new())` becomes `None` on the child
/// because the value is indistinguishable from an empty static
/// attribute `<… leaf=""/>`. Use a sentinel or a separate flag if
/// you need to round-trip a real empty string; the same caveat
/// applies to the explicit-list flatten path.
impl<T: PropValue> PropValue for Option<T> {
    fn to_prop_js(&self) -> JsValue {
        match self {
            Some(v) => v.to_prop_js(),
            None => JsValue::NULL,
        }
    }
    fn from_prop_js(value: JsValue) -> Option<Self> {
        if value.is_null() || value.is_undefined() {
            return Some(None);
        }
        if value.as_string().as_deref() == Some("") {
            return Some(None);
        }
        T::from_prop_js(value).map(Some)
    }
    fn prop_static_kind() -> StaticPropKind {
        T::prop_static_kind()
    }
}

/// Implemented by `#[derive(Props)]` structs.
///
/// `prop_leaves()` returns the static slice of leaf names — the
/// `#[component]` macro consumes this to extend a component's
/// `keys()` and to dispatch `get`/`set`/`is_prop` for a bare
/// `#[prop(flatten)]` / `#[model(flatten)]` field. `prop_get` /
/// `prop_set` route directly through each field's `PropValue` impl
/// (no whole-struct serde round-trip). `prop_static_kind` reports
/// the exact static-attribute kind for each leaf, removing the
/// runtime "probe the serialised default" heuristic the explicit-list
/// flatten path uses.
pub trait Props {
    fn prop_leaves() -> &'static [&'static str];
    fn prop_get(&self, leaf: &str) -> JsValue;
    fn prop_set(&mut self, leaf: &str, value: JsValue);
    fn prop_static_kind(leaf: &str) -> StaticPropKind;
}
