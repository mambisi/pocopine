//! `PineSwitch` — toggle control, `role="switch"`.
//!
//! Two-way bindable with `pp-model="enabled"`: on click, toggles
//! `checked` and fires `pp:update:model` with the new boolean.
//! Renders the `data-state` attribute (`"checked"` / `"unchecked"`)
//! for styling.
//!
//! ```html
//! <pine-switch pp-model="dark_mode"></pine-switch>
//! ```

use pocopine::prelude::*;
use pocopine::tick;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSwitch.poco")]
pub struct PineSwitch {
    pub checked: bool,
    pub disabled: bool,
}

#[handlers]
impl PineSwitch {
    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        self.checked = !self.checked;
        emit_checked_changed(self.checked);
    }
}

/// Fire `pp:update:model` from the current element, deferred via
/// tick::next so `pp-model`'s parent→child mirror effect doesn't
/// re-enter this scope's `&mut self` borrow while `toggle` is
/// still on the stack.
fn emit_checked_changed(checked: bool) {
    let Some(el) = pocopine_core::scope::current_el() else { return };
    tick::next(move || {
        let init = web_sys::CustomEventInit::new();
        init.set_bubbles(true);
        init.set_detail(&JsValue::from_bool(checked));
        if let Ok(ev) =
            web_sys::CustomEvent::new_with_event_init_dict("pp:update:model", &init)
        {
            let _ = el.dispatch_event(&ev);
        }
    });
}
