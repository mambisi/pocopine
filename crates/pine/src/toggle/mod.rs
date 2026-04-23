//! `<pine-toggle>` — standalone two-state button.
//!
//! Mirrors reka-ui / Radix `<Toggle>`. A single button with an
//! `aria-pressed` state that flips on click. Same mechanics as
//! Switch but with button semantics (think "bold" / "italic" in a
//! formatting toolbar) rather than a form-toggle. Emits
//! `pp:update:pressed` so `pp-model:pressed="my_bool"` two-way
//! binds naturally.
//!
//! ```html
//! <pine-toggle pp-model:pressed="bold">
//!   <strong>B</strong>
//! </pine-toggle>
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineToggle.poco", role = "interactive")]
pub struct PineToggle {
    /// Two-way bindable via `pp-model:pressed="bool"`.
    #[model] pub pressed: bool,
    #[prop] pub disabled: bool,
}

#[handlers]
impl PineToggle {
    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        self.pressed = !self.pressed;
    }
}
