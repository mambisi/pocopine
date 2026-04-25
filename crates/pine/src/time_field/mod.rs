//! `<pine-time-field>` — custom-segmented time input. Port of
//! reka-ui's `TimeFieldRoot` + `TimeFieldInput` pair, collapsed
//! into a single primitive with its HH / MM (and optional SS)
//! segments rendered internally.
//!
//! Value is a plain `HH:MM` / `HH:MM:SS` string so the field
//! drops into any form that previously used `<input type="time">`.
//! The segment state machine lives in [`segments`]; this module
//! wires keyboard events to it and emits `pp:update:value` when
//! the time commits.
//!
//! v1 scope:
//! - 24-hour cycle only. `hourCycle: 12` + AM / PM toggle deferred.
//! - Granularity is minute (HH:MM) or second (HH:MM:SS), driven by
//!   the `step` prop — minute when `step == 60` (native default)
//!   or `0`, second when `0 < step < 60`, minute otherwise.
//! - No timezone segment, no `isTimeUnavailable`.

pub mod segments;

use pocopine::prelude::*;
use pocopine::{current_scope_id, emit_model_field, refs};
use segments::{TimePart, TimeSegments};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

#[derive(Serialize, Deserialize)]
#[component(
    template = "PineTimeField.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineTimeField {
    /// Two-way bound time as `HH:MM` / `HH:MM:SS`. Empty = unset.
    #[model]
    pub value: String,
    /// Earliest selectable time (compared lexicographically against
    /// `value` — works because segments are fixed-width).
    #[prop]
    pub min_value: String,
    #[prop]
    pub max_value: String,
    /// Step in seconds. `60` / `0` → minute precision; any value
    /// in `(0, 60)` reveals the seconds segment.
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

    /// Currently-focused segment — `"hour"` / `"minute"` /
    /// `"second"` / `""`.
    pub active_part: String,
    /// Focus selects the whole segment, so the first digit
    /// replaces it rather than appending to the committed value.
    pub part_selected: bool,

    pub hour_display: String,
    pub minute_display: String,
    pub second_display: String,
    pub has_seconds: bool,

    pub filled: bool,
    pub invalid: bool,
    pub focused: bool,
    pub touched: bool,
}

impl Default for PineTimeField {
    fn default() -> Self {
        let mut f = Self {
            value: String::new(),
            min_value: String::new(),
            max_value: String::new(),
            step: 0.0,
            name: String::new(),
            disabled: false,
            readonly: false,
            required: false,
            active_part: String::new(),
            part_selected: false,
            hour_display: TimePart::Hour.placeholder().into(),
            minute_display: TimePart::Minute.placeholder().into(),
            second_display: TimePart::Second.placeholder().into(),
            has_seconds: false,
            filled: false,
            invalid: false,
            focused: false,
            touched: false,
        };
        f.sync_display(&TimeSegments::default());
        f
    }
}

#[handlers]
impl PineTimeField {
    fn on_setup(&mut self) {
        self.has_seconds = seconds_visible(self.step);
        let segs = TimeSegments::parse(&self.value);
        self.sync_display(&segs);
    }

    #[watch(value)]
    fn on_value_change(&mut self, new: String, _prev: Option<String>) {
        let segs = TimeSegments::parse(&new);
        self.sync_display(&segs);
    }

    #[watch(step)]
    fn on_step_change(&mut self, new: f64, _prev: Option<f64>) {
        self.has_seconds = seconds_visible(new);
        // Re-derive filled/invalid for the new granularity.
        let segs = TimeSegments::parse(&self.value);
        self.sync_display(&segs);
    }

    // ── focus bookkeeping ──────────────────────────────────────

    pub fn focus_hour(&mut self) {
        self.set_active_part(Some(TimePart::Hour), true);
    }
    pub fn focus_minute(&mut self) {
        self.set_active_part(Some(TimePart::Minute), true);
    }
    pub fn focus_second(&mut self) {
        self.set_active_part(Some(TimePart::Second), true);
    }

    pub fn on_blur(&mut self, ev: web_sys::FocusEvent) {
        if self.blur_stays_within_field(&ev) {
            return;
        }
        self.set_active_part(None, false);
        self.touched = true;
    }

    // ── keydown dispatcher ─────────────────────────────────────

    pub fn on_key(&mut self, ev: web_sys::KeyboardEvent) {
        let Some(part) = self.part_from_event(&ev).or_else(|| self.active_part()) else {
            return;
        };
        self.set_active_part(Some(part), self.part_selected);
        if self.disabled || self.readonly {
            return;
        }

        let key = ev.key();
        let mut segs = TimeSegments::parse(&self.value);
        // Preserve partial digits not yet committed to `value`.
        self.refill_from_display(&mut segs);

        match key.as_str() {
            "ArrowLeft" => {
                ev.prevent_default();
                if let Some(prev) = part.prev(self.has_seconds) {
                    self.set_active_part(Some(prev), true);
                    self.focus_segment_in_dom(prev);
                }
                return;
            }
            "ArrowRight" => {
                ev.prevent_default();
                if let Some(next) = part.next(self.has_seconds) {
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
            "Tab" => return,
            "Enter" => {
                ev.prevent_default();
                if let Some(next) = part.next(self.has_seconds) {
                    self.set_active_part(Some(next), true);
                    self.focus_segment_in_dom(next);
                }
                return;
            }
            k if k.len() == 1 => {
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
                    if let Some(next) = part.next(self.has_seconds) {
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

impl PineTimeField {
    fn set_active_part(&mut self, part: Option<TimePart>, selected: bool) {
        self.active_part = part.map(TimePart::as_str).unwrap_or("").into();
        self.focused = part.is_some();
        self.part_selected = part.is_some() && selected;
    }

    fn active_part(&self) -> Option<TimePart> {
        match self.active_part.as_str() {
            "hour" => Some(TimePart::Hour),
            "minute" => Some(TimePart::Minute),
            "second" if self.has_seconds => Some(TimePart::Second),
            _ => None,
        }
    }

    fn part_from_event(&self, ev: &web_sys::KeyboardEvent) -> Option<TimePart> {
        let el = ev
            .current_target()
            .or_else(|| ev.target())
            .and_then(|t| t.dyn_into::<web_sys::Element>().ok())?;
        match el.get_attribute("data-part").as_deref() {
            Some("hour") => Some(TimePart::Hour),
            Some("minute") => Some(TimePart::Minute),
            Some("second") if self.has_seconds => Some(TimePart::Second),
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

    /// Pull partial digits from the rendered placeholders — the
    /// user may have typed "1" in the hour without committing a
    /// valid `HH:MM` value yet.
    fn refill_from_display(&self, s: &mut TimeSegments) {
        if s.hour.is_none() {
            if let Ok(h) = self.hour_display.trim().parse::<u8>() {
                if h <= 23 {
                    s.hour = Some(h);
                }
            }
        }
        if s.minute.is_none() {
            if let Ok(m) = self.minute_display.trim().parse::<u8>() {
                if m <= 59 {
                    s.minute = Some(m);
                }
            }
        }
        if self.has_seconds && s.second.is_none() {
            if let Ok(sec) = self.second_display.trim().parse::<u8>() {
                if sec <= 59 {
                    s.second = Some(sec);
                }
            }
        }
    }

    fn commit_segments(&mut self, s: &TimeSegments) {
        self.sync_display(s);
        let next = s.to_value(self.has_seconds).unwrap_or_default();
        if next != self.value {
            self.value = next.clone();
            emit_model_field("value", &self.value);
        }
    }

    fn sync_display(&mut self, s: &TimeSegments) {
        self.hour_display = s.display(TimePart::Hour);
        self.minute_display = s.display(TimePart::Minute);
        self.second_display = s.display(TimePart::Second);
        self.filled = s.is_complete(self.has_seconds);
        self.invalid = self.compute_invalid(s);
    }

    fn compute_invalid(&self, s: &TimeSegments) -> bool {
        let Some(v) = s.to_value(self.has_seconds) else {
            return false;
        };
        if !self.min_value.is_empty() && v.as_str() < self.min_value.as_str() {
            return true;
        }
        if !self.max_value.is_empty() && v.as_str() > self.max_value.as_str() {
            return true;
        }
        false
    }

    fn focus_segment_in_dom(&self, part: TimePart) {
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

fn seconds_visible(step: f64) -> bool {
    step > 0.0 && step < 60.0
}
