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
//! Emits `pp:update:value` whenever `self.value` changes so
//! `pp-model:value` round-trips to the parent.

use pocopine::prelude::*;
use pocopine::{current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{HtmlElement, HtmlInputElement, KeyboardEvent};

/// Per-slot view model. `slots` is rebuilt only when `length`
/// changes, so the keyed `pp-for` reuses the same `<input>`
/// DOM nodes across every keystroke — focus stays where we put
/// it instead of getting reconciled away.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct OtpSlot {
    pub index: u32,
    pub aria_label: String,
}

#[derive(Serialize, Deserialize)]
#[component(template = "PineOtpField.poco", role = "panel")]
pub struct PineOtpField {
    #[prop] pub length: u32,
    #[model] pub value: String,
    #[prop] pub r#type: String,
    #[prop] pub mask: bool,
    #[prop] pub disabled: bool,
    #[prop] pub label: String,
    /// Recomputed when `length` changes.
    pub slots: Vec<OtpSlot>,
    /// Mobile-keyboard hint, derived from `type`.
    pub input_mode: String,
    /// HTML `pattern` attr, derived from `type`.
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

    pub fn on_ready(&self) {
        // Slot `<input>`s exist now — mirror the initial value
        // onto their `.value` properties.
        self.sync_slot_display();
    }

    #[watch(value)]
    fn on_value_change(&mut self, _: String, _: Option<String>) {
        self.sync_slot_display();
    }
    #[watch(length)]
    fn on_length_change(&mut self, _: u32, _: Option<u32>) {
        self.rebuild_slots();
    }
    #[watch(r#type)]
    fn on_type_change(&mut self, _: String, _: Option<String>) {
        self.refresh_mode();
    }
    #[watch(mask)]
    fn on_mask_change(&mut self, _: bool, _: Option<bool>) {
        self.sync_slot_display();
    }

    /// `@input` on each slot. The browser has already written the
    /// typed character (or pasted chunk) into the slot's `.value`;
    /// read it off the event target, fold it into `self.value`,
    /// and advance focus to the next slot.
    pub fn on_slot_input(&mut self, ev: web_sys::Event, index: u32) {
        let Some(el) = ev
            .target()
            .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        let typed = el.value();
        let focus_target = self.apply_input(index as usize, &typed).unwrap_or(index);
        self.sync_slot_display();
        // Always re-focus: the reactive flush after a handler run
        // has `pp-for` re-emit `insert_before` on every keyed clone,
        // which blurs whichever slot we're on. Targeting the intended
        // slot after the flush lands focus correctly.
        self.focus_slot(focus_target);
    }

    /// `@keydown.backspace` — the key modifier filters out every
    /// other keystroke. When the slot is empty we swallow the
    /// default (which would do nothing useful) and walk focus
    /// back a slot, clearing the previous character. When the
    /// slot has a character, we let the browser clear it in
    /// place — the `@input` listener picks up the deletion on
    /// the next pass.
    pub fn on_slot_backspace(&mut self, ev: KeyboardEvent, index: u32) {
        let Some(el) = ev
            .target()
            .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
        else {
            return;
        };
        if !el.value().is_empty() {
            return;
        }
        ev.prevent_default();
        let target = self.apply_backspace(index as usize).unwrap_or(index);
        self.sync_slot_display();
        self.focus_slot(target);
    }

    /// `@keydown.left.prevent` / `@keydown.right.prevent` —
    /// move focus across slots without moving the caret inside
    /// the current single-char input.
    pub fn focus_delta(&self, index: u32, dir: i32) {
        let n = self.length as i32;
        if n <= 0 {
            return;
        }
        let target = (index as i32 + dir).clamp(0, n - 1) as u32;
        self.focus_slot(target);
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

    /// Push `self.value` + the `mask` setting into every slot's
    /// live `.value` + `data-filled` attribute. We go through the
    /// `.value` DOM *property* (not the `value` attribute) because
    /// after a user types, the property and the attribute diverge
    /// and only the property is what the browser renders.
    fn sync_slot_display(&self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(root_el) = refs::get_on(scope, "root") else {
            return;
        };
        let value_chars: Vec<char> = self.value.chars().collect();
        let len = self.length as usize;
        for i in 0..len {
            let ch = value_chars
                .get(i)
                .copied()
                .map(|c| c.to_string())
                .unwrap_or_default();
            let filled = !ch.is_empty();
            let display = if self.mask && filled {
                "\u{2022}".to_string()
            } else {
                ch
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

    /// Schedule focus on the slot at `target` after the current
    /// reactive flush completes. `pp-for`'s keyed reconcile calls
    /// `Node::insertBefore` on every reused clone even when the
    /// position is unchanged — that move blurs any focused input
    /// inside it. Deferring the focus write until after the flush
    /// lands it on a clone that won't get moved out from under us.
    fn focus_slot(&self, target: u32) {
        let Some(scope) = current_scope_id() else { return };
        pocopine::tick::after_flush(move || {
            let Some(root_el) = refs::get_on(scope, "root") else { return };
            let selector = format!("input[data-index=\"{target}\"]");
            if let Some(el) = root_el.query_selector(&selector).ok().flatten() {
                if let Ok(html) = el.dyn_into::<HtmlElement>() {
                    let _ = html.focus();
                }
            }
        });
    }

    /// Dense-prefix semantics: `self.value` is always a contiguous
    /// prefix of filled slots. Types at slot `index` replace-or-append
    /// (clamped to the fill prefix); deletes / backspaces truncate
    /// so everything at or past this position empties. Matches
    /// standard OTP UX — the user can only fill / rewind from the
    /// right edge.
    fn apply_input(&mut self, index: usize, typed: &str) -> Option<u32> {
        let filtered = self.filter(typed);
        let typed_chars: Vec<char> = filtered.chars().collect();
        let len = self.length as usize;
        let mut value_chars: Vec<char> = self.value.chars().collect();

        if typed_chars.is_empty() {
            if typed.is_empty() {
                // Delete / Backspace on a filled slot — clear
                // every slot at or past this position.
                value_chars.truncate(index);
                self.value = value_chars.iter().collect();
                return None;
            }
            // Filter rejected the character — revert the DOM,
            // leave `value` alone, focus stays.
            return None;
        }

        // Clamp the insertion point to the end of the current fill
        // prefix: typing at slot 5 when only slots 0-2 are filled
        // should write into slot 3 (next empty), not leave a hole.
        let write_at = index.min(value_chars.len());
        let advance = typed_chars.len();

        for (i, c) in typed_chars.iter().enumerate() {
            let slot = write_at + i;
            if slot >= len {
                break;
            }
            if slot < value_chars.len() {
                value_chars[slot] = *c;
            } else {
                value_chars.push(*c);
            }
        }

        value_chars.truncate(len);
        self.value = value_chars.iter().collect();
        Some((write_at + advance).min(len.saturating_sub(1)) as u32)
    }

    fn apply_backspace(&mut self, index: usize) -> Option<u32> {
        if index == 0 {
            return None;
        }
        // Walk focus back and clear every slot from the previous
        // slot onwards — same dense-prefix semantics as
        // `apply_input`'s delete branch.
        let target = index - 1;
        let mut value_chars: Vec<char> = self.value.chars().collect();
        value_chars.truncate(target);
        self.value = value_chars.iter().collect();
        Some(target as u32)
    }

    fn filter(&self, s: &str) -> String {
        match self.r#type.as_str() {
            "alphanumeric" => s.chars().filter(|c| c.is_ascii_alphanumeric()).collect(),
            _ => s.chars().filter(|c| c.is_ascii_digit()).collect(),
        }
    }
}
