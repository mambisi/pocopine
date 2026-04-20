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

/// One visible slot. Exposed to the template via `pp-for`.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct OtpSlot {
    pub index: u32,
    /// Real character at this position, `""` when the slot is
    /// empty. Drives `data-filled` + used as the source of truth
    /// for `display` when `mask` is off.
    pub char: String,
    /// What the DOM `<input>` shows. Mirrors `char`, or `"•"`
    /// when `mask` is `true` and the slot is filled.
    pub display: String,
    pub aria_label: String,
}

#[derive(Serialize, Deserialize)]
#[component(template = "PineOtpField.poco", role = "panel")]
pub struct PineOtpField {
    #[prop]
    pub length: u32,
    #[prop]
    pub value: String,
    #[prop]
    pub r#type: String,
    #[prop]
    pub mask: bool,
    #[prop]
    pub disabled: bool,
    #[prop]
    pub label: String,
    /// Computed view model for `pp-for` in the template.
    pub slots: Vec<OtpSlot>,
    /// `inputmode` derived from `type`. Flows to each slot's
    /// `inputmode` attribute for mobile keyboards.
    pub input_mode: String,
    /// Validation pattern derived from `type` — used by the
    /// native `pattern` attribute + the in-Rust input filter.
    pub pattern: String,
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
        }
    }
}

#[handlers]
impl PineOtpField {
    pub fn on_setup(&mut self) {
        self.refresh_mode();
        self.refresh_slots();
    }

    /// Keep the view model in sync when the parent writes a new
    /// `value` externally (e.g. via `pp-bind` or a `pp-model`
    /// round-trip). Emits no events — only repaints.
    #[watch(value)]
    fn on_value_change(&mut self, _value: String, _prev: Option<String>) {
        self.refresh_slots();
    }

    /// Same story for `length`, `type`, `mask`: rebuild the view
    /// model so slots re-render with the new configuration.
    #[watch(length)]
    fn on_length_change(&mut self, _v: u32, _prev: Option<u32>) {
        self.refresh_slots();
    }

    #[watch(mask)]
    fn on_mask_change(&mut self, _v: bool, _prev: Option<bool>) {
        self.refresh_slots();
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        let Some(root) = refs.get("root") else { return };

        // ── `input` — a character landed in a slot. The browser
        // already replaced the slot's value, so we pull it, feed
        // it through the component's filter + set-at-index
        // logic, and advance focus.
        let h = handle.clone();
        let root_for_input = root.clone();
        let input_cb = Closure::wrap(Box::new(move |ev: Event| {
            let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) else {
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
            let new_value = h.with(|s| s.value.clone());
            pocopine::emit_from(&root_for_input, "pp:update:model", new_value);
        }) as Box<dyn FnMut(Event)>);
        let target: &EventTarget = root.as_ref();
        let _ = target.add_event_listener_with_callback("input", input_cb.as_ref().unchecked_ref());
        input_cb.forget();

        // ── Backspace on an empty slot walks focus back and
        // clears the previous filled slot. Without this, the
        // user gets stuck on an empty slot after fixing a
        // mistake.
        let h2 = handle;
        let root_for_kd = root.clone();
        let target_kd: EventTarget = root.into();
        let kd_cb = Closure::wrap(Box::new(move |ev: KeyboardEvent| {
            let key = ev.key();
            if key != "Backspace" && key != "ArrowLeft" && key != "ArrowRight" {
                return;
            }
            let Some(el) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) else {
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
                    let new_value = h2.with(|s| s.value.clone());
                    pocopine::emit_from(&root_for_kd, "pp:update:model", new_value);
                }
                "ArrowLeft" => {
                    if idx > 0 {
                        ev.prevent_default();
                        h2.with(|s| s.focus_slot(idx - 1));
                    }
                }
                "ArrowRight" => {
                    ev.prevent_default();
                    h2.with(|s| s.focus_slot(idx + 1));
                }
                _ => {}
            }
        }) as Box<dyn FnMut(KeyboardEvent)>);
        let _ = target_kd
            .add_event_listener_with_callback("keydown", kd_cb.as_ref().unchecked_ref());
        kd_cb.forget();
    }
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
                // numeric (default)
                self.input_mode = "numeric".into();
                self.pattern = "[0-9]*".into();
            }
        }
    }

    fn refresh_slots(&mut self) {
        let len = self.length as usize;
        let value_chars: Vec<char> = self.value.chars().collect();
        self.slots = (0..len)
            .map(|i| {
                let ch = value_chars.get(i).copied();
                let char_s = ch.map(|c| c.to_string()).unwrap_or_default();
                let display = if self.mask && !char_s.is_empty() {
                    "\u{2022}".to_string()
                } else {
                    char_s.clone()
                };
                OtpSlot {
                    index: i as u32,
                    char: char_s.clone(),
                    display,
                    aria_label: format!("Digit {}", i + 1),
                }
            })
            .collect();
        // `:value` attribute bindings don't overwrite an <input>'s
        // live `.value` property once the user (or test) has typed
        // into it. Force-sync the property from the current slot
        // state so cases where the *component* authoritatively
        // changes the value (backspace clearing a previous slot,
        // multi-char paste spread, rejected-char revert) land in
        // the DOM.
        self.sync_slot_properties();
    }

    fn sync_slot_properties(&self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(root_el) = refs::get_on(scope, "root") else { return };
        for slot in &self.slots {
            let selector = format!("input[data-index=\"{}\"]", slot.index);
            let Some(el) = root_el.query_selector(&selector).ok().flatten() else {
                continue;
            };
            if let Ok(input) = el.dyn_into::<HtmlInputElement>() {
                if input.value() != slot.display {
                    input.set_value(&slot.display);
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
                // Remove from the dense value.
                if index < value_chars.len() {
                    value_chars.remove(index);
                }
            } else {
                // A disallowed character was typed (e.g. "a" in a
                // numeric-mode slot). Leave `value` alone and push
                // the old slot content back to the DOM by
                // refreshing the view model — this reverts the
                // browser-side change the keystroke caused.
                self.refresh_slots();
                return;
            }
        } else if typed_chars.len() == 1 {
            let c = typed_chars[0];
            if index < value_chars.len() {
                value_chars[index] = c;
            } else if value_chars.len() < len {
                // Append — only valid when the slot is exactly
                // at the current tail. Clicking into a
                // far-ahead empty slot and typing is a no-op
                // except for pushing at the tail, which matches
                // user expectation (auto-advance implies dense
                // from-the-front fill).
                value_chars.push(c);
            }
            // Advance focus to the next slot, clamped.
            let next = (index + 1).min(len.saturating_sub(1));
            self.focus_slot(next);
        } else {
            // Multi-char arrival — paste or one-time-code
            // autofill. Spread the characters across slots
            // starting at `index`.
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
            let next = (index + typed_chars.len()).min(len.saturating_sub(1));
            self.focus_slot(next);
        }

        value_chars.truncate(len);
        self.value = value_chars.iter().collect();
        self.refresh_slots();
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
        self.refresh_slots();
        self.focus_slot(target);
    }

    /// Filter a string down to characters allowed by `type`.
    /// Applied to both direct typing and paste content.
    fn filter(&self, s: &str) -> String {
        match self.r#type.as_str() {
            "alphanumeric" => s.chars().filter(|c| c.is_ascii_alphanumeric()).collect(),
            _ => s.chars().filter(|c| c.is_ascii_digit()).collect(),
        }
    }

    /// Move DOM focus to the slot at `index`, if it exists.
    /// Uses a selector against the root so it still works after
    /// `pp-for` re-renders (refs inside a loop get clobbered).
    fn focus_slot(&self, index: usize) {
        let Some(scope) = current_scope_id() else { return };
        let Some(root_el) = refs::get_on(scope, "root") else { return };
        let selector = format!("input[data-index=\"{index}\"]");
        let found: Option<web_sys::Element> =
            root_el.query_selector(&selector).ok().flatten();
        if let Some(el) = found {
            if let Ok(html) = el.dyn_into::<HtmlElement>() {
                let _ = html.focus();
            }
        }
    }
}
