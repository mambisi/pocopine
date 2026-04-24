//! `<pine-date-field>` — native `<input type="date">` wrapped as a
//! Pine primitive with `Option<DateValue>` two-way binding.
//!
//! The browser's built-in date input handles segment editing,
//! keyboard nav, locale-specific format, and min/max validation
//! for free. Pine's layer on top:
//!
//! - `Option<DateValue>` model binding — empty input ↔ `None`,
//!   valid ISO ↔ `Some(DateValue)`. Authors never touch strings.
//! - `data-focused` / `data-filled` / `data-invalid` hooks that
//!   match the Field primitive's vocabulary so composing a
//!   `<pine-field-root>` around it keeps the same state contract.
//! - Forward `disabled` / `readonly` / `required` / `name` to the
//!   native input so form submission + browser validation keeps
//!   working.
//!
//! A fancier segmented implementation (`<pine-date-field-root>` +
//! per-segment triggers à la reka's `DateFieldRoot`) can land
//! later without breaking the `<pine-date-field>` tag — the
//! native-input version remains the drop-in.

use crate::datetime::DateValue;
use pocopine::prelude::*;
use pocopine::{current_scope_id, emit_model_field, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

#[derive(Serialize, Deserialize)]
#[component(template = "PineDateField.poco", role = "interactive", display = "contents")]
pub struct PineDateField {
    /// Two-way bound date. `None` = empty input.
    #[model]
    pub value: Option<DateValue>,
    /// Earliest selectable date (native `min` attribute).
    #[prop]
    pub min_value: Option<DateValue>,
    /// Latest selectable date (native `max` attribute).
    #[prop]
    pub max_value: Option<DateValue>,
    /// `name` attribute for form submission.
    #[prop]
    pub name: String,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub readonly: bool,
    #[prop]
    pub required: bool,

    /// `true` while the native input holds focus.
    pub focused: bool,
    /// `true` once the user has blurred the input at least once.
    pub touched: bool,
    /// `true` when `value` is `Some`.
    pub filled: bool,
}

impl Default for PineDateField {
    fn default() -> Self {
        Self {
            value: None,
            min_value: None,
            max_value: None,
            name: String::new(),
            disabled: false,
            readonly: false,
            required: false,
            focused: false,
            touched: false,
            filled: false,
        }
    }
}

#[handlers]
impl PineDateField {
    pub fn on_setup(&mut self) {
        self.filled = self.value.is_some();
    }

    pub fn on_ready(&self) {
        // Seed the native input's `.value` — the template's
        // `:value` binding writes the HTML attribute, which
        // doesn't round-trip after the user types.
        if let Some(input) = resolve_input() {
            let wanted = value_as_iso(&self.value);
            if input.value() != wanted {
                input.set_value(&wanted);
            }
        }
    }

    /// Watcher for external writes to `value` (via
    /// `pp-model:value` or a parent's mutation). Mirror the new
    /// value into the native input so the displayed segments
    /// stay in sync.
    #[watch(value)]
    fn on_value_change(&mut self, new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        self.filled = new.is_some();
        let Some(input) = resolve_input() else {
            return;
        };
        let wanted = value_as_iso(&new);
        if input.value() != wanted {
            input.set_value(&wanted);
        }
    }

    pub fn on_focus(&mut self) {
        self.focused = true;
    }

    pub fn on_blur(&mut self) {
        self.focused = false;
        self.touched = true;
    }

    pub fn on_input(&mut self) {
        let Some(input) = resolve_input() else {
            return;
        };
        let raw = input.value();
        let next = if raw.is_empty() {
            None
        } else {
            DateValue::parse_iso(&raw)
        };
        self.filled = next.is_some();
        if self.value != next {
            self.value = next;
            emit_model_field("value", &self.value);
        }
    }
}

fn resolve_input() -> Option<HtmlInputElement> {
    let scope = current_scope_id()?;
    let el = refs::get_on(scope, "root")?;
    el.dyn_into::<HtmlInputElement>().ok()
}

fn value_as_iso(v: &Option<DateValue>) -> String {
    v.map(|d| d.to_string()).unwrap_or_default()
}
