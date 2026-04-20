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
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSwitch.poco")]
pub struct PineSwitch {
    #[prop] pub checked: bool,
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineSwitch {
    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        self.checked = !self.checked;
        emit("pp:update:model", self.checked);
    }
}
