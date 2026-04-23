//! `<pine-input>` — native text input with built-in field-state
//! tracking.
//!
//! A single-part primitive that renders a native `<input>` and
//! mirrors interaction state onto `data-*` attributes the same
//! way [`PineField`](crate::field) does, so the same CSS hooks
//! work whether the input is standalone or wrapped in a Field.
//!
//! Props:
//!
//! - `type` — HTML input type. Default `"text"`.
//! - `value` — current value. Two-way bindable via
//!   `pp-model:value="my_string"`.
//! - `placeholder`, `name` — forwarded to the native attributes.
//! - `disabled`, `required`, `readonly` — forwarded to the
//!   native boolean attributes and mirrored on `data-disabled`.
//!
//! Auto-tracked state (exposed as `data-*`):
//!
//! - `data-focused` — `true` while the input holds focus.
//! - `data-touched` — `true` once the input has been blurred.
//! - `data-dirty` — `true` once the user has typed.
//! - `data-filled` — `true` when the current value is non-empty.
//!
//! ```html
//! <pine-input pp-model:value="name"
//!             placeholder="Enter your name"></pine-input>
//! ```
//!
//! Composes with Field — dropping a `<pine-input>` inside a
//! `<pine-field-control>` lets Field apply its `id` /
//! `aria-describedby` / `aria-invalid` wiring while the input
//! keeps tracking its own state.

use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

#[derive(Serialize, Deserialize)]
#[component(template = "PineInput.poco", role = "interactive", display = "contents")]
pub struct PineInput {
    /// HTML input `type`. Default `"text"`.
    #[prop] pub r#type: String,
    /// Current value. Two-way bindable via
    /// `pp-model:value="my_field"`.
    #[model] pub value: String,
    #[prop] pub placeholder: String,
    #[prop] pub name: String,
    #[prop] pub disabled: bool,
    #[prop] pub required: bool,
    #[prop] pub readonly: bool,

    pub focused: bool,
    pub touched: bool,
    pub dirty: bool,
    pub filled: bool,
}

impl Default for PineInput {
    fn default() -> Self {
        Self {
            r#type: "text".into(),
            value: String::new(),
            placeholder: String::new(),
            name: String::new(),
            disabled: false,
            required: false,
            readonly: false,
            focused: false,
            touched: false,
            dirty: false,
            filled: false,
        }
    }
}

#[handlers]
impl PineInput {
    pub fn on_setup(&mut self) {
        if self.r#type.is_empty() {
            self.r#type = "text".into();
        }
        self.filled = !self.value.is_empty();
    }

    pub fn on_ready(&self) {
        // Seed the DOM input's `.value` property from `self.value`.
        // The template's `:value` binding would only touch the HTML
        // attribute, which doesn't round-trip after the user types.
        let Some(scope) = current_scope_id() else { return };
        let Some(el) = refs::get_on(scope, "root") else { return };
        let Ok(input) = el.dyn_into::<HtmlInputElement>() else { return };
        if input.value() != self.value {
            input.set_value(&self.value);
        }
    }

    /// External writes to `value` (via `pp-bind:value` /
    /// `pp-model:value`) flow into the DOM `.value` property.
    /// Guarded so round-tripping a user keystroke doesn't
    /// clobber cursor position.
    #[watch(value)]
    fn on_value_change(&mut self, value: String, _prev: Option<String>) {
        let Some(scope) = current_scope_id() else { return };
        let Some(el) = refs::get_on(scope, "root") else { return };
        let Ok(input) = el.dyn_into::<HtmlInputElement>() else { return };
        if input.value() != value {
            input.set_value(&value);
        }
        self.filled = !value.is_empty();
    }

    pub fn on_focus(&mut self) {
        self.focused = true;
    }

    pub fn on_blur(&mut self) {
        self.focused = false;
        self.touched = true;
    }

    pub fn on_input(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(el) = refs::get_on(scope, "root") else { return };
        let Ok(input) = el.dyn_into::<HtmlInputElement>() else { return };
        let v = input.value();
        self.dirty = true;
        self.filled = !v.is_empty();
        if self.value != v {
            self.value = v;
        }
    }
}
