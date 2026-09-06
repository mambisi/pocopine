//! Host type-checking of shared component plans. Server rendering consumes
//! explicit locale/message parts; it never invokes browser DOM evaluators.
use super::TranslationPlan;
use crate::ScopeId;
use crate::expr::RootAccess;
use wasm_bindgen::JsValue;
use web_sys::Element;

pub fn value(_: &'static TranslationPlan, _: &JsValue, _: Option<&RootAccess>) -> JsValue {
    panic!("browser translation expressions cannot execute on the host")
}
pub fn install(_: &Element, _: ScopeId, _: &JsValue, _: &'static TranslationPlan) {
    panic!("browser translation plans cannot mount on the host")
}
