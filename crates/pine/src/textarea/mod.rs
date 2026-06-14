//! `<pine-textarea>` — native textarea with model binding,
//! field-state tracking, and optional content autosizing.
//!
//! Mirrors [`PineInput`](crate::input::PineInput) for multiline
//! text surfaces. It renders a native `<textarea>`, keeps the DOM
//! `.value` property in sync with the `value` model field, and
//! exposes interaction state via `data-*` attributes.
//!
//! Props:
//!
//! - `value` — current value. Two-way bindable via
//!   `pp-model:value="my_string"`.
//! - `placeholder`, `name`, `rows` — forwarded to the native
//!   attributes. `rows` defaults to `2`.
//! - `disabled`, `required`, `readonly` — forwarded to native
//!   boolean attributes and mirrored on `data-disabled`.
//! - `autosize` — when true, the textarea height follows its
//!   content. Authors can still cap the size with CSS `max-height`
//!   and `overflow-y`.
//!
//! ```html
//! <pine-textarea pp-model:value="body"
//!                placeholder="Take a note..."
//!                autosize></pine-textarea>
//! ```

use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlTextAreaElement;

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineTextarea.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineTextarea {
    /// Current value. Two-way bindable via
    /// `pp-model:value="my_field"`.
    #[model]
    pub value: String,
    #[prop]
    pub placeholder: String,
    #[prop]
    pub name: String,
    #[prop]
    pub rows: u32,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub required: bool,
    #[prop]
    pub readonly: bool,
    #[prop]
    pub autosize: bool,

    pub focused: bool,
    pub touched: bool,
    pub dirty: bool,
    pub filled: bool,
}

impl Default for PineTextarea {
    fn default() -> Self {
        Self {
            value: String::new(),
            placeholder: String::new(),
            name: String::new(),
            rows: 2,
            disabled: false,
            required: false,
            readonly: false,
            autosize: false,
            focused: false,
            touched: false,
            dirty: false,
            filled: false,
        }
    }
}

#[handlers]
impl PineTextarea {
    fn on_setup(&mut self) {
        if self.rows == 0 {
            self.rows = 2;
        }
        self.filled = !self.value.is_empty();
    }

    fn on_ready(&self) {
        let Some(textarea) = root_textarea() else {
            return;
        };
        if textarea.value() != self.value {
            textarea.set_value(&self.value);
        }
        if self.autosize {
            autosize_textarea(&textarea);
        }
    }

    /// External writes to `value` flow into the DOM `.value`
    /// property. Guarded so user keystrokes do not clobber cursor
    /// position while the model round-trips.
    #[watch(value)]
    fn on_value_change(&mut self, value: String, _prev: Option<String>) {
        let Some(textarea) = root_textarea() else {
            return;
        };
        if textarea.value() != value {
            textarea.set_value(&value);
        }
        self.filled = !value.is_empty();
        if self.autosize {
            autosize_textarea(&textarea);
        }
    }

    pub fn on_focus(&mut self) {
        self.focused = true;
        if self.autosize
            && let Some(textarea) = root_textarea()
        {
            autosize_textarea(&textarea);
        }
    }

    pub fn on_blur(&mut self) {
        self.focused = false;
        self.touched = true;
    }

    pub fn on_input(&mut self) {
        let Some(textarea) = root_textarea() else {
            return;
        };
        let value = textarea.value();
        self.dirty = true;
        self.filled = !value.is_empty();
        if self.autosize {
            autosize_textarea(&textarea);
        }
        if self.value != value {
            self.value = value;
        }
    }
}

fn root_textarea() -> Option<HtmlTextAreaElement> {
    let scope = current_scope_id()?;
    let el = refs::get_on(scope, "root")?;
    el.dyn_into::<HtmlTextAreaElement>().ok()
}

fn autosize_textarea(textarea: &HtmlTextAreaElement) {
    let style = textarea.style();
    let _ = style.set_property("height", "auto");
    let height = textarea.scroll_height();
    if height > 0 {
        let _ = style.set_property("height", &format!("{height}px"));
    }
}
