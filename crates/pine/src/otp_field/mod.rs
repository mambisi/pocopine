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
/// `length` prop. Kept deliberately free of per-character state:
/// `.value`, `data-filled`, and the bullet-vs-char display are
/// synced imperatively in [`PineOtpField::sync_slot_display`] so
/// the `pp-for` that renders the slots never has to reconcile on
/// every keystroke (which would swap the focused `<input>` out
/// from under the user).
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
        self.rebuild_slots();
    }

    pub fn on_ready_pull(&self) {}

    /// When the parent writes `value` externally (via `pp-bind`
    /// or a `pp-model` round-trip), push the new characters into
    /// the live `<input>` elements. Skips touching `self.slots`
    /// to avoid pp-for reconciliation — the slots only change
    /// structurally with `length`.
    #[watch(value)]
    fn on_value_change(&mut self, _value: String, _prev: Option<String>) {
        self.sync_slot_display();
    }

    /// `length` is structural — re-materialise the slot list.
    #[watch(length)]
    fn on_length_change(&mut self, _v: u32, _prev: Option<u32>) {
        self.rebuild_slots();
    }

    /// Likewise `type` — the `inputmode` / `pattern` mirrors
    /// depend on it.
    #[watch(r#type)]
    fn on_type_change(&mut self, _v: String, _prev: Option<String>) {
        self.refresh_mode();
    }

    /// `mask` only affects `.value` (bullet vs real char); no
    /// slot rebuild needed.
    #[watch(mask)]
    fn on_mask_change(&mut self, _v: bool, _prev: Option<bool>) {
        self.sync_slot_display();
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        // First-paint sync — the template's `pp-for` has just
        // materialised the slot <input>s; push the current
        // value (if any) to their `.value` properties + stamp
        // `data-filled` so authors' CSS rules match on mount.
        handle.with(|s| s.sync_slot_display());
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
    /// `.value` properties and `data-filled` attribute. Doesn't
    /// mutate `self.slots`, so the `pp-for` that renders them
    /// doesn't reconcile on every keystroke — keeping the
    /// focused `<input>` stable.
    fn sync_slot_display(&self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(root_el) = refs::get_on(scope, "root") else { return };
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
                // Remove from the dense value.
                if index < value_chars.len() {
                    value_chars.remove(index);
                }
            } else {
                // A disallowed character was typed (e.g. "a" in a
                // numeric-mode slot). Leave `value` alone and push
                // the old slot content back to the DOM so the
                // rejected keystroke doesn't linger.
                self.sync_slot_display();
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
        // `#[watch(value)]` fires sync_slot_display, but the
        // reactive flush happens after Handle::update returns —
        // call it eagerly so any follow-up logic (focus_slot,
        // emit) runs against an up-to-date DOM.
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
    /// Deferred to a macrotask (`setTimeout(_, 0)`) so reactive
    /// state flush — which can re-run `pp-for` bindings and drop
    /// focus during node reuse — has completed before we try to
    /// land on the target input.
    fn focus_slot(&self, index: usize) {
        let Some(scope) = current_scope_id() else { return };
        let cb = Closure::once_into_js(Box::new(move || {
            let Some(root_el) = refs::get_on(scope, "root") else {
                return;
            };
            let selector = format!("input[data-index=\"{index}\"]");
            if let Some(el) = root_el.query_selector(&selector).ok().flatten() {
                if let Ok(html) = el.dyn_into::<HtmlElement>() {
                    let _ = html.focus();
                }
            }
        }) as Box<dyn FnOnce()>);
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(
                cb.unchecked_ref(),
                0,
            );
        }
    }
}
