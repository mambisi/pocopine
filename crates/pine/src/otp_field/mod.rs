//! `<pine-otp-field>` — one-time-password input with auto-advance.
//!
//! A fixed-length code entry (typically 4 – 6 digits). Each slot
//! is a single-character `<input>` that auto-advances focus on
//! type, walks back on Backspace-while-empty, and accepts pastes
//! that spread across the remaining slots.
//!
//! Props:
//!
//! - `length` — number of slots. Default `6`.
//! - `value` — current code. Two-way bindable via
//!   `pp-model:value="my_code"`.
//! - `type` — `"numeric"` (default) or `"alphanumeric"`.
//!   Controls the slot `inputmode` / `pattern` and the input
//!   filter applied to typing + paste.
//! - `mask` — when `true`, slots display a bullet (`•`) instead
//!   of the actual character. The `value` field still holds the
//!   real code.
//! - `disabled` — disables every slot.
//! - `label` — accessible group label (the `<input>` elements
//!   are children of a `role="group"` container that carries
//!   `aria-label`).
//!
//! ```html
//! <pine-otp-field length="6"
//!                 pp-model:value="code"
//!                 label="Verification code"></pine-otp-field>
//! ```
//!
//! On complete entry, the component emits `pp:update:model`
//! with the current value so `pp-model` round-trips to the
//! parent.

use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{Event, EventTarget, HtmlElement, HtmlInputElement, KeyboardEvent};

/// Structural descriptor for one slot — driven entirely by the
/// `length` prop. Per-character state (`.value`, `data-filled`,
/// bullet-vs-char display) is applied imperatively in
/// [`PineOtpField::sync_slot_display`] so the `pp-for` that
/// renders the slots never has to reconcile on every keystroke
/// (which would swap the focused `<input>` out from under the
/// user).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct OtpSlot {
    pub index: u32,
    pub aria_label: String,
}

#[derive(Serialize, Deserialize)]
#[component(template = "PineOtpField.poco", role = "panel")]
pub struct PineOtpField {
    #[prop]
    pub length: u32,
    #[model]
    pub value: String,
    #[prop]
    pub r#type: String,
    #[prop]
    pub mask: bool,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub label: String,
    /// Computed view model for `pp-for` in the template. Only
    /// rebuilt when `length` changes.
    pub slots: Vec<OtpSlot>,
    /// `inputmode` derived from `type`. Flows to each slot's
    /// `inputmode` attribute for mobile keyboards.
    pub input_mode: String,
    /// Validation pattern derived from `type` — used by the
    /// native `pattern` attribute.
    pub pattern: String,
    /// Target slot index for the next scheduled focus move.
    /// `handle_input` / `handle_backspace` write here; the event
    /// closure consumes it and schedules the actual `.focus()`
    /// on the next macrotask — after any reactive flush has
    /// finished walking the template.
    pub pending_focus: Option<u32>,
}

impl Default for PineOtpField {
    fn default() -> Self {
        Self {
            length: 6,
            value: String::new(),
            r#type: "numeric".into(),
            mask: false,
            disabled: false,
            label: "One-time code".into(),
            slots: Vec::new(),
            input_mode: "numeric".into(),
            pattern: "[0-9]*".into(),
            pending_focus: None,
        }
    }
}

#[handlers]
impl PineOtpField {
    pub fn on_setup(&mut self) {
        self.refresh_mode();
        self.rebuild_slots();
    }

    /// When the parent writes `value` externally (via `pp-bind`
    /// or a `pp-model` round-trip), push the new characters into
    /// the live `<input>` elements.
    #[watch(value)]
    fn on_value_change(&mut self, _value: String, _prev: Option<String>) {
        self.sync_slot_display();
    }

    /// `length` is structural — re-materialise the slot list.
    #[watch(length)]
    fn on_length_change(&mut self, _v: u32, _prev: Option<u32>) {
        self.rebuild_slots();
    }

    /// `type` changes the `inputmode` / `pattern` mirrors.
    #[watch(r#type)]
    fn on_type_change(&mut self, _v: String, _prev: Option<String>) {
        self.refresh_mode();
    }

    /// `mask` only affects `.value` display (bullet vs real char).
    #[watch(mask)]
    fn on_mask_change(&mut self, _v: bool, _prev: Option<bool>) {
        self.sync_slot_display();
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        // First-paint sync — the template's `pp-for` has just
        // materialised the slot <input>s; push the current value
        // (if any) to their `.value` properties.
        handle.with(|s| s.sync_slot_display());
        let Some(root) = refs.get("root") else { return };

        // ── `input` — a character landed in a slot. The browser
        // already updated the slot's .value, so we pull it, feed
        // it through the component's filter + set-at-index logic,
        // and remember where focus should land.
        let h = handle.clone();
        let root_for_input = root.clone();
        let input_cb = Closure::wrap(Box::new(move |ev: Event| {
            let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            else {
                return;
            };
            let Some(idx) = el
                .get_attribute("data-index")
                .and_then(|s| s.parse::<usize>().ok())
            else {
                return;
            };
            let typed = el.value();
            h.update(|s| s.handle_input(idx, &typed));
            // Consume the requested focus target, falling back to
            // the slot the event fired on so a deletion keeps
            // focus put.
            let focus_target =
                h.update(|s| s.pending_focus.take()).unwrap_or(idx as u32);

            // `h.update` can trigger reactive flushes that walk
            // the template and blur the focused input. Re-apply
            // focus via a macrotask so it lands after flush.
            schedule_focus(root_for_input.clone(), focus_target);
        }) as Box<dyn FnMut(Event)>);
        let target: &EventTarget = root.as_ref();
        let _ =
            target.add_event_listener_with_callback("input", input_cb.as_ref().unchecked_ref());
        input_cb.forget();

        // ── Keydown: Backspace on empty slot walks back; arrows
        // navigate between slots. Regular character keys are
        // handled by the browser + the `input` listener above.
        let h2 = handle;
        let root_for_kd = root.clone();
        let target_kd: EventTarget = root.into();
        let kd_cb = Closure::wrap(Box::new(move |ev: KeyboardEvent| {
            let key = ev.key();
            if key != "Backspace" && key != "ArrowLeft" && key != "ArrowRight" {
                return;
            }
            let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
            else {
                return;
            };
            let Some(idx) = el
                .get_attribute("data-index")
                .and_then(|s| s.parse::<usize>().ok())
            else {
                return;
            };
            match key.as_str() {
                "Backspace" => {
                    if !el.value().is_empty() {
                        return;
                    }
                    ev.prevent_default();
                    h2.update(|s| s.handle_backspace(idx));
                    let focus_target = h2
                        .update(|s| s.pending_focus.take())
                        .unwrap_or(idx as u32);
                    schedule_focus(root_for_kd.clone(), focus_target);
                }
                "ArrowLeft" => {
                    if idx > 0 {
                        ev.prevent_default();
                        schedule_focus(root_for_kd.clone(), (idx - 1) as u32);
                    }
                }
                "ArrowRight" => {
                    ev.prevent_default();
                    schedule_focus(root_for_kd.clone(), (idx + 1) as u32);
                }
                _ => {}
            }
        }) as Box<dyn FnMut(KeyboardEvent)>);
        let _ =
            target_kd.add_event_listener_with_callback("keydown", kd_cb.as_ref().unchecked_ref());
        kd_cb.forget();
    }
}

/// Schedule a `.focus()` on `input[data-index="{target}"]`
/// inside `root` on the next macrotask (`setTimeout(_, 0)`).
/// Used by all handler paths so the re-focus lands *after*
/// any reactive flush from a preceding `Handle::update` has
/// finished walking the template.
fn schedule_focus(root: web_sys::Element, target: u32) {
    pocopine::tick::after_flush(move || {
        let selector = format!("input[data-index=\"{target}\"]");
        if let Some(found) = root.query_selector(&selector).ok().flatten() {
            if let Ok(html) = found.dyn_into::<HtmlElement>() {
                let _ = html.focus();
            }
        }
    });
}

// ── Non-handler helpers ───────────────────────────────────────────

impl PineOtpField {
    fn refresh_mode(&mut self) {
        match self.r#type.as_str() {
            "alphanumeric" => {
                self.input_mode = "text".into();
                self.pattern = "[a-zA-Z0-9]*".into();
            }
            _ => {
                self.input_mode = "numeric".into();
                self.pattern = "[0-9]*".into();
            }
        }
    }

    fn rebuild_slots(&mut self) {
        let len = self.length as usize;
        self.slots = (0..len)
            .map(|i| OtpSlot {
                index: i as u32,
                aria_label: format!("Digit {}", i + 1),
            })
            .collect();
    }

    /// Push the current `value` + `mask` into the DOM slots'
    /// `.value` properties and `data-filled` attribute without
    /// mutating `self.slots`, so the `pp-for` doesn't reconcile.
    fn sync_slot_display(&self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(root_el) = refs::get_on(scope, "root") else {
            return;
        };
        let value_chars: Vec<char> = self.value.chars().collect();
        let len = self.length as usize;
        for i in 0..len {
            let char_s = value_chars
                .get(i)
                .copied()
                .map(|c| c.to_string())
                .unwrap_or_default();
            let filled = !char_s.is_empty();
            let display = if self.mask && filled {
                "\u{2022}".to_string()
            } else {
                char_s
            };
            let selector = format!("input[data-index=\"{i}\"]");
            let Some(el) = root_el.query_selector(&selector).ok().flatten() else {
                continue;
            };
            let _ = el.set_attribute("data-filled", if filled { "true" } else { "false" });
            if let Ok(input) = el.dyn_into::<HtmlInputElement>() {
                if input.value() != display {
                    input.set_value(&display);
                }
            }
        }
    }

    fn handle_input(&mut self, index: usize, typed: &str) {
        let filtered = self.filter(typed);
        let typed_chars: Vec<char> = filtered.chars().collect();
        let len = self.length as usize;
        let mut value_chars: Vec<char> = self.value.chars().collect();

        if typed_chars.is_empty() {
            if typed.is_empty() {
                // User genuinely cleared the slot (cut / delete).
                if index < value_chars.len() {
                    value_chars.remove(index);
                }
            } else {
                // Disallowed character rejected by the filter —
                // revert the DOM input + leave `value` alone.
                self.sync_slot_display();
                return;
            }
        } else if typed_chars.len() == 1 {
            let c = typed_chars[0];
            if index < value_chars.len() {
                value_chars[index] = c;
            } else if value_chars.len() < len {
                value_chars.push(c);
            }
            self.pending_focus = Some(((index + 1).min(len.saturating_sub(1))) as u32);
        } else {
            // Multi-char arrival — paste or autofill. Spread.
            for (i, c) in typed_chars.iter().enumerate() {
                let slot = index + i;
                if slot >= len {
                    break;
                }
                if slot < value_chars.len() {
                    value_chars[slot] = *c;
                } else {
                    value_chars.push(*c);
                }
            }
            self.pending_focus =
                Some(((index + typed_chars.len()).min(len.saturating_sub(1))) as u32);
        }

        value_chars.truncate(len);
        self.value = value_chars.iter().collect();
        // `#[watch(value)]` fires sync_slot_display on the next
        // reactive tick, but we also call it eagerly so subsequent
        // logic sees the up-to-date DOM.
        self.sync_slot_display();
    }

    fn handle_backspace(&mut self, index: usize) {
        if index == 0 {
            return;
        }
        let mut value_chars: Vec<char> = self.value.chars().collect();
        let target = index.saturating_sub(1);
        if target < value_chars.len() {
            value_chars.remove(target);
        }
        self.value = value_chars.iter().collect();
        self.sync_slot_display();
        self.pending_focus = Some(target as u32);
    }

    /// Filter a string down to characters allowed by `type`.
    fn filter(&self, s: &str) -> String {
        match self.r#type.as_str() {
            "alphanumeric" => s.chars().filter(|c| c.is_ascii_alphanumeric()).collect(),
            _ => s.chars().filter(|c| c.is_ascii_digit()).collect(),
        }
    }
}
