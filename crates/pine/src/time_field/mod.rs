//! `<pine-time-field>` — native `<input type="time">` wrapped as a
//! Pine primitive. Value is a plain `HH:MM` (or `HH:MM:SS`) string
//! — the browser's own widget handles segment editing, AM / PM
//! toggling for 12-hour locales, and min / max / step validation.
//!
//! Mirrors [`crate::date_field::PineDateField`]'s prop + state
//! vocabulary (focused / touched / filled) so the two compose
//! identically inside `<pine-field-root>`.

use pocopine::prelude::*;
use pocopine::{current_scope_id, emit_model_field, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

#[derive(Serialize, Deserialize)]
#[component(template = "PineTimeField.poco", role = "interactive", display = "contents")]
pub struct PineTimeField {
    /// Two-way bound time as `HH:MM` / `HH:MM:SS`. Empty = unset.
    #[model]
    pub value: String,
    /// Earliest selectable time (native `min` attribute).
    #[prop]
    pub min_value: String,
    /// Latest selectable time (native `max` attribute).
    #[prop]
    pub max_value: String,
    /// Step in seconds. `60` (default native) = minute precision;
    /// `1` shows the seconds segment.
    #[prop]
    pub step: f64,
    #[prop]
    pub name: String,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub readonly: bool,
    #[prop]
    pub required: bool,

    pub focused: bool,
    pub touched: bool,
    pub filled: bool,
}

impl Default for PineTimeField {
    fn default() -> Self {
        Self {
            value: String::new(),
            min_value: String::new(),
            max_value: String::new(),
            step: 0.0,
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
impl PineTimeField {
    pub fn on_setup(&mut self) {
        self.filled = !self.value.is_empty();
    }

    pub fn on_ready(&self) {
        if let Some(input) = resolve_input() {
            if input.value() != self.value {
                input.set_value(&self.value);
            }
        }
    }

    #[watch(value)]
    fn on_value_change(&mut self, new: String, _prev: Option<String>) {
        self.filled = !new.is_empty();
        let Some(input) = resolve_input() else {
            return;
        };
        if input.value() != new {
            input.set_value(&new);
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
        self.filled = !raw.is_empty();
        if self.value != raw {
            self.value = raw.clone();
            emit_model_field("value", raw);
        }
    }
}

fn resolve_input() -> Option<HtmlInputElement> {
    let scope = current_scope_id()?;
    let el = refs::get_on(scope, "root")?;
    el.dyn_into::<HtmlInputElement>().ok()
}
