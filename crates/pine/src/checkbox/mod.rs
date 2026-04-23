//! `PineCheckbox` — tri-state checkbox, `role="checkbox"`.
//!
//! `state` is one of `"checked"`, `"unchecked"`, or
//! `"indeterminate"`. Click cycles
//! `unchecked → checked → unchecked` (with `indeterminate →
//! checked`). Fires `pp:update:model` with the new string on
//! every change; pair with `pp-model="state"` on the tag for
//! two-way binding against a parent `String` field.
//!
//! ```html
//! <pine-checkbox pp-model="agree"></pine-checkbox>
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

/// Initial state defaults to `"unchecked"` (via our manual Default
/// impl; the macro-generated `#[derive(Default)]` would produce an
/// empty string which renders as a non-checkbox state).
#[derive(Serialize, Deserialize)]
#[component(template = "PineCheckbox.poco", role = "interactive")]
pub struct PineCheckbox {
    #[model]
    pub state: String,
    #[prop]
    pub disabled: bool,
}

impl Default for PineCheckbox {
    fn default() -> Self {
        Self {
            state: "unchecked".into(),
            disabled: false,
        }
    }
}

#[handlers]
impl PineCheckbox {
    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        self.state = match self.state.as_str() {
            "checked" => "unchecked".into(),
            _ => "checked".into(), // unchecked OR indeterminate → checked
        };
    }
}
