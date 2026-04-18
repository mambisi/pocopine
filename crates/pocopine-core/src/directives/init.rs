//! `pp-init="handler"` — run a handler once after the scope is bound.

use js_sys::Array;

use super::DirectiveCall;
use crate::scope::{invoke_handler, with_current_el};

pub fn run(call: &DirectiveCall) {
    let el = call.el.clone();
    let handler = call.value.clone();
    let scope_id = call.scope_id;
    with_current_el(&el, || {
        invoke_handler(scope_id, &handler, &Array::new());
    });
}
