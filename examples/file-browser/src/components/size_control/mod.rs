//! `<file-browser-size-control>` — CFE size input with a range handle.

mod display;

use display::SizeDisplay;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

#[derive(Serialize, Deserialize)]
#[component(
    template = "FileBrowserSizeControl.poco",
    role = "panel",
    display = "block"
)]
pub struct FileBrowserSizeControl {
    #[model]
    pub value: f64,
    #[prop]
    pub label: String,
    #[prop]
    pub min: f64,
    #[prop]
    pub max: f64,
    #[prop]
    pub disabled: bool,
    pub unit: String,
}

impl Default for FileBrowserSizeControl {
    fn default() -> Self {
        Self {
            value: 1.0,
            label: String::new(),
            min: 1.0,
            max: 1024.0,
            disabled: false,
            unit: "MB".to_string(),
        }
    }
}

impl FileBrowserSizeControl {
    fn current_display(&self) -> SizeDisplay {
        Self::display(self.value, self.min, self.max, &self.unit)
    }

    fn sync_inputs(&self) {
        sync_inputs(self.current_display());
    }
}

fn sync_inputs(display: SizeDisplay) {
    // :value initializes attributes; edited native controls need property writes.
    for (name, value) in [("range", display.value), ("amount", display.amount)] {
        if let Some(input) = pocopine::refs::get_as::<web_sys::HtmlInputElement>(name) {
            if input.value_as_number() != value {
                input.set_value_as_number(value);
            }
        }
    }
    if let Some(select) = pocopine::refs::get_as::<web_sys::HtmlSelectElement>("unit") {
        select.set_value(&display.unit);
    }
}

#[handlers]
impl FileBrowserSizeControl {
    #[computed]
    fn display(value: f64, min: f64, max: f64, unit: &str) -> SizeDisplay {
        SizeDisplay::new(value, min, max, unit)
    }

    fn on_ready(&self) {
        self.sync_inputs();
    }

    #[watch(value, min, max, unit)]
    fn on_display_change(
        value: FieldUpdate<f64>,
        min: FieldUpdate<f64>,
        max: FieldUpdate<f64>,
        unit: FieldUpdate<String>,
    ) {
        sync_inputs(SizeDisplay::new(
            value.current,
            min.current,
            max.current,
            &unit.current,
        ));
    }

    pub fn set_range(&mut self, event: web_sys::Event) {
        if !self.disabled
            && let Some(value) = input_value(event)
        {
            self.value = self.current_display().accept_range(value);
            self.unit = self.current_display().unit;
        }
        // Also restore rejected/no-op edits, which do not trigger a watcher.
        self.sync_inputs();
    }

    pub fn set_amount(&mut self, event: web_sys::Event) {
        if !self.disabled
            && let Some(amount) = input_value(event)
        {
            self.value = self.current_display().accept_amount(amount);
            self.unit = self.current_display().unit;
        }
        self.sync_inputs();
    }

    pub fn set_unit(&mut self, event: web_sys::Event) {
        if !self.disabled
            && let Some(select) = event
                .target()
                .and_then(|t| t.dyn_into::<web_sys::HtmlSelectElement>().ok())
        {
            (self.value, self.unit) = self.current_display().accept_unit(&select.value());
        }
        self.sync_inputs();
    }
}

fn input_value(event: web_sys::Event) -> Option<f64> {
    event
        .target()?
        .dyn_into::<web_sys::HtmlInputElement>()
        .ok()
        .map(|input| input.value_as_number())
}
