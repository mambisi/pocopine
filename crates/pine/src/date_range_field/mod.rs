//! `<pine-date-range-field>` — two paired `<pine-date-field>`s for
//! a `start` / `end` pair. The outer primitive owns the pair
//! state + a small bounds-aware mirror (writing `start` also
//! updates the inner end-field's `min`, and vice versa) so a
//! picked range can't invert on itself.
//!
//! Composes `<pine-date-field>` twice rather than re-implementing
//! segment editing — native `<input type="date">` already handles
//! the keyboard side.

use crate::datetime::DateValue;
use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, KeyboardEvent};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineDateRangeField.poco",
    role = "interactive",
    display = "contents",
    uses = [crate::date_field::PineDateField],
)]
pub struct PineDateRangeField {
    #[model]
    pub start: Option<DateValue>,
    #[model]
    pub end: Option<DateValue>,
    /// Lower bound propagated to **both** inner fields as their
    /// native `min`. The end-field's effective min is clamped
    /// further up to the current `start` (see `effective_end_min`).
    #[prop]
    pub min_value: Option<DateValue>,
    /// Upper bound propagated to **both** inner fields as their
    /// native `max`. The start-field's effective max is clamped
    /// down to the current `end` (see `effective_start_max`).
    #[prop]
    pub max_value: Option<DateValue>,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub readonly: bool,
    #[prop]
    pub required: bool,
    #[prop]
    pub start_name: String,
    #[prop]
    pub end_name: String,
    #[prop]
    pub separator: String,
}

#[handlers]
impl PineDateRangeField {
    fn on_mount(&mut self) {
        if self.separator.is_empty() {
            self.separator = " – ".into();
        }
    }

    pub fn on_key(&mut self, ev: KeyboardEvent) {
        if ev.key() != "Tab" {
            return;
        }
        let Some(target) = ev.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        let part = target.get_attribute("data-part");
        // Class-only selector: the `pine-date-range-field-{start,end}`
        // class is authored on the inner `<pine-date-field>` custom
        // tag, but the mount strips fallthrough class/style off the
        // outer custom tag after forwarding them onto the rendered
        // template root. So the class lives on the inner
        // `<div class="pine-date-field">`, not on the
        // `<pine-date-field>` custom element. A combined
        // `pine-date-field.pine-date-range-field-start` selector
        // matches nothing post-strip; a class-only selector matches
        // the rendered root, which is what we want.
        let in_start = target
            .closest(".pine-date-range-field-start")
            .ok()
            .flatten()
            .is_some();
        let in_end = target
            .closest(".pine-date-range-field-end")
            .ok()
            .flatten()
            .is_some();

        if !ev.shift_key() && in_start && part.as_deref() == Some("year") {
            ev.prevent_default();
            self.focus_field_segment("end-field", "segment-month");
        } else if ev.shift_key() && in_end && part.as_deref() == Some("month") {
            ev.prevent_default();
            self.focus_field_segment("start-field", "segment-year");
        }
    }

    /// End-field min: the greater of the author's `min_value` and
    /// the currently picked `start`. Keeps the user from picking
    /// an end that precedes start.
    #[computed]
    fn effective_end_min(
        min_value: &Option<DateValue>,
        start: &Option<DateValue>,
    ) -> Option<DateValue> {
        match (*min_value, *start) {
            (Some(m), Some(s)) if s > m => Some(s),
            (None, Some(s)) => Some(s),
            (m, _) => m,
        }
    }

    /// Start-field max: the lesser of the author's `max_value`
    /// and the currently picked `end`.
    #[computed]
    fn effective_start_max(
        max_value: &Option<DateValue>,
        end: &Option<DateValue>,
    ) -> Option<DateValue> {
        match (*max_value, *end) {
            (Some(m), Some(e)) if e < m => Some(e),
            (None, Some(e)) => Some(e),
            (m, _) => m,
        }
    }
}

impl PineDateRangeField {
    fn focus_field_segment(&self, field_ref: &str, segment_selector: &str) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(field) = refs::get_on(scope, field_ref) else {
            return;
        };
        let selector = match segment_selector {
            "segment-month" => "[data-part=\"month\"]",
            "segment-day" => "[data-part=\"day\"]",
            "segment-year" => "[data-part=\"year\"]",
            _ => return,
        };
        let Some(segment) = field.query_selector(selector).ok().flatten() else {
            return;
        };
        if let Ok(html) = segment.dyn_into::<HtmlElement>() {
            let _ = html.focus();
        }
    }
}
