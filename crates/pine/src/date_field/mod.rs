//! `<pine-date-field>` — custom-segmented date input in the reka
//! shape: three editable segments (MM / DD / YYYY) with digit
//! typing, arrow-key increment/decrement, and auto-advance.
//!
//! Port of reka-ui's `DateFieldRoot` + `DateFieldInput` pair,
//! collapsed into a single primitive with the segments rendered
//! internally. The pure segment state machine lives in
//! [`segments`]; the component below wires keyboard events to
//! it, mirrors `Option<DateValue>` props in/out of the segments,
//! and emits `pp:update:value` whenever a complete date commits.
//!
//! v1 scope relative to reka:
//! - Fixed `MM / DD / YYYY` order. Locale-specific orders
//!   (DD/MM/YYYY, YYYY/MM/DD) come later.
//! - No granularity beyond day. Time segments live in the sibling
//!   `<pine-time-field>` primitive.
//! - No timezone segment, no `isDateUnavailable`, no `hideTimeZone`.
//!
//! Composes with `<pine-field-root>` unchanged — the `data-focused`
//! / `data-touched` / `data-filled` / `data-invalid` hooks match
//! the Field vocabulary.

pub mod segments;

use crate::datetime::DateValue;
use pocopine::prelude::*;
use pocopine::{current_scope_id, emit_model_field, refs};
use segments::{DatePart, DateSegments};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineDateField.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineDateField {
    /// Two-way bound date. `None` while any segment is empty or
    /// the combination is invalid.
    #[model]
    pub value: Option<DateValue>,
    /// Earliest selectable date. Currently surfaced only as
    /// `data-invalid` on the host; enforcement is soft.
    #[prop]
    pub min_value: Option<DateValue>,
    #[prop]
    pub max_value: Option<DateValue>,
    #[prop]
    pub name: String,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub readonly: bool,
    #[prop]
    pub required: bool,

    /// Currently-focused segment — one of `"month"` / `"day"` /
    /// `"year"` / `""` (nothing focused). Template uses this to
    /// flip `data-focused="true"` on the right segment.
    pub active_part: String,
    /// Focus selects the whole segment, so the first digit
    /// replaces it rather than appending to the committed value.
    pub part_selected: bool,

    /// Rendered display strings for each segment — either a
    /// placeholder (`"MM"` / `"DD"` / `"YYYY"`) or the committed
    /// digits. Templates read these via `pp-text`.
    pub month_display: String,
    pub day_display: String,
    pub year_display: String,

    pub filled: bool,
    pub invalid: bool,
    pub focused: bool,
    pub touched: bool,
}

impl Default for PineDateField {
    fn default() -> Self {
        let mut f = Self {
            value: None,
            min_value: None,
            max_value: None,
            name: String::new(),
            disabled: false,
            readonly: false,
            required: false,
            active_part: String::new(),
            part_selected: false,
            month_display: DatePart::Month.placeholder().into(),
            day_display: DatePart::Day.placeholder().into(),
            year_display: DatePart::Year.placeholder().into(),
            filled: false,
            invalid: false,
            focused: false,
            touched: false,
        };
        f.sync_display(&DateSegments::default());
        f
    }
}

#[handlers]
impl PineDateField {
    pub fn on_setup(&mut self) {
        let segs = self.segments_from_value();
        self.sync_display(&segs);
    }

    #[watch(value)]
    fn on_value_change(&mut self, new: Option<DateValue>, _prev: Option<Option<DateValue>>) {
        let segs = match new {
            Some(d) => DateSegments::from_date(d),
            None => DateSegments::default(),
        };
        self.sync_display(&segs);
    }

    // ── focus bookkeeping (fired by @focus on each segment) ──

    pub fn focus_month(&mut self) {
        self.set_active_part(Some(DatePart::Month), true);
    }
    pub fn focus_day(&mut self) {
        self.set_active_part(Some(DatePart::Day), true);
    }
    pub fn focus_year(&mut self) {
        self.set_active_part(Some(DatePart::Year), true);
    }

    pub fn on_blur(&mut self, ev: web_sys::FocusEvent) {
        if self.blur_stays_within_field(&ev) {
            return;
        }
        self.set_active_part(None, false);
        self.touched = true;
    }

    // ── single keydown entrypoint ──────────────────────────

    /// `@keydown.prevent` on every segment dispatches here. We
    /// take the whole `KeyboardEvent` so the same handler covers
    /// digits, arrows, Backspace, and Tab without a zoo of
    /// modifier-specific entries.
    pub fn on_key(&mut self, ev: web_sys::KeyboardEvent) {
        let Some(part) = self.part_from_event(&ev).or_else(|| self.active_part()) else {
            return;
        };
        self.set_active_part(Some(part), self.part_selected);
        if self.disabled || self.readonly {
            return;
        }

        let key = ev.key();
        let mut segs = self.segments_from_value();

        match key.as_str() {
            "ArrowLeft" => {
                ev.prevent_default();
                if let Some(prev) = part.prev() {
                    self.set_active_part(Some(prev), true);
                    self.focus_segment_in_dom(prev);
                }
                return;
            }
            "ArrowRight" => {
                ev.prevent_default();
                if let Some(next) = part.next() {
                    self.set_active_part(Some(next), true);
                    self.focus_segment_in_dom(next);
                }
                return;
            }
            "ArrowUp" => {
                ev.prevent_default();
                segs.increment(part);
            }
            "ArrowDown" => {
                ev.prevent_default();
                segs.decrement(part);
            }
            "Backspace" => {
                ev.prevent_default();
                segs.backspace(part);
            }
            "Tab" => {
                // Let the browser move focus via native tab order.
                return;
            }
            "Enter" => {
                ev.prevent_default();
                if let Some(next) = part.next() {
                    self.set_active_part(Some(next), true);
                    self.focus_segment_in_dom(next);
                }
                return;
            }
            k if k.len() == 1 => {
                // Single-character keys — only digits count.
                let ch = k.chars().next().unwrap();
                let Some(digit) = ch.to_digit(10) else {
                    return;
                };
                ev.prevent_default();
                if self.part_selected {
                    segs.clear_part(part);
                }
                let advanced = segs.type_digit(part, digit as u8);
                self.part_selected = false;
                self.commit_segments(&segs);
                if advanced {
                    if let Some(next) = part.next() {
                        self.set_active_part(Some(next), true);
                        self.focus_segment_in_dom(next);
                    }
                }
                return;
            }
            _ => return,
        }

        self.commit_segments(&segs);
    }
}

impl PineDateField {
    fn set_active_part(&mut self, part: Option<DatePart>, selected: bool) {
        self.active_part = part.map(DatePart::as_str).unwrap_or("").into();
        self.focused = part.is_some();
        self.part_selected = part.is_some() && selected;
    }

    fn active_part(&self) -> Option<DatePart> {
        match self.active_part.as_str() {
            "month" => Some(DatePart::Month),
            "day" => Some(DatePart::Day),
            "year" => Some(DatePart::Year),
            _ => None,
        }
    }

    fn part_from_event(&self, ev: &web_sys::KeyboardEvent) -> Option<DatePart> {
        let el = ev
            .current_target()
            .or_else(|| ev.target())
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())?;
        match el.get_attribute("data-part").as_deref() {
            Some("month") => Some(DatePart::Month),
            Some("day") => Some(DatePart::Day),
            Some("year") => Some(DatePart::Year),
            _ => None,
        }
    }

    fn blur_stays_within_field(&self, ev: &web_sys::FocusEvent) -> bool {
        let Some(scope) = current_scope_id() else {
            return false;
        };
        let Some(root) = refs::get_on(scope, "root") else {
            return false;
        };
        let Some(next) = ev.related_target() else {
            return false;
        };
        let Ok(next_node) = next.dyn_into::<web_sys::Node>() else {
            return false;
        };
        root.contains(Some(&next_node))
    }

    fn segments_from_value(&self) -> DateSegments {
        match self.value {
            Some(d) => DateSegments::from_date(d),
            None => {
                let mut s = DateSegments::default();
                // Preserve what's already shown — the user may
                // have typed partial digits before the value
                // became complete.
                if let Ok(m) = self.month_display.trim().parse::<u8>() {
                    s.month = Some(m);
                }
                if let Ok(d) = self.day_display.trim().parse::<u8>() {
                    s.day = Some(d);
                }
                if let Ok(y) = self.year_display.trim().parse::<i32>() {
                    s.year = Some(y);
                }
                s
            }
        }
    }

    /// Called after any transition — writes the new display
    /// strings, recomputes `filled` / `invalid` / `value`, and
    /// emits `pp:update:value` if the committed date changed.
    fn commit_segments(&mut self, s: &DateSegments) {
        self.sync_display(s);
        let next = s.to_date();
        if next != self.value {
            self.value = next;
            emit_model_field("value", self.value);
        }
    }

    fn sync_display(&mut self, s: &DateSegments) {
        self.month_display = s.display(DatePart::Month);
        self.day_display = s.display(DatePart::Day);
        self.year_display = s.display(DatePart::Year);
        self.filled = s.is_complete();
        self.invalid = self.compute_invalid(s);
    }

    fn compute_invalid(&self, s: &DateSegments) -> bool {
        let Some(d) = s.to_date() else {
            // Incomplete is not invalid — it's just pending.
            return false;
        };
        if let Some(min) = self.min_value {
            if d < min {
                return true;
            }
        }
        if let Some(max) = self.max_value {
            if d > max {
                return true;
            }
        }
        false
    }

    fn focus_segment_in_dom(&self, part: DatePart) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let name = format!("segment-{}", part.as_str());
        let Some(el) = refs::get_on(scope, &name) else {
            return;
        };
        if let Ok(html) = el.dyn_into::<HtmlElement>() {
            let _ = html.focus();
        }
    }
}
