//! `<pine-radio-group-*>` — accessible single-choice radio group.
//!
//! Form primitive. Root owns the selected value and provides its
//! scope id to Items; each Item is a `role="radio"` button. Clicking
//! or pressing Space on an Item writes the new value into Root and
//! emits `pp:update:model`, so `pp-model:value` on the Root tag
//! round-trips naturally.
//!
//! Arrow-key navigation via `pp-roving.horizontal` — authors can
//! swap orientation by passing `orientation="vertical"` (reflected
//! as `data-orientation` for styling / roving direction).
//!
//! ```html
//! <pine-radio-group-root pp-model:value="plan" name="plan">
//!   <pine-radio-group-item value="free">
//!     <pine-radio-group-indicator>●</pine-radio-group-indicator>
//!     Free
//!   </pine-radio-group-item>
//!   <pine-radio-group-item value="pro">
//!     <pine-radio-group-indicator>●</pine-radio-group-indicator>
//!     Pro
//!   </pine-radio-group-item>
//! </pine-radio-group-root>
//! ```

use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, provide, watch_scope_field};
use pocopine_core::reactive::ScopeId;
use pocopine_core::scope::Scope;
use serde::{Deserialize, Serialize};

const ROOT_KEY: &str = "pine-radio-group-root";
/// Key under which an Item publishes its own scope id so a nested
/// Indicator can mirror its `checked` field. Matches the pattern
/// used by DropdownMenu's CheckboxItem / RadioItem (same key name
/// kept deliberately — both compounds expose the same
/// "indicator watches its owner item" contract).
const CHECKED_OWNER_KEY: &str = "pine-checked-owner";

// ── Root ──────────────────────────────────────────────────────────

/// Radio group container. Owns `value`; provides its scope id so
/// Items can read + write it through `Scope::find` + Handle::update.
#[derive(Serialize, Deserialize)]
#[component(template = "PineRadioGroupRoot.poco")]
pub struct PineRadioGroupRoot {
    pub value: String,
    /// `"horizontal"` (default) or `"vertical"`. Drives both the
    /// `data-orientation` attribute and the roving direction —
    /// horizontal uses Arrow-Left / Arrow-Right, vertical uses
    /// Arrow-Up / Arrow-Down.
    pub orientation: String,
    /// Disables every Item when true. Individual Items can also be
    /// disabled via their own `disabled` prop.
    pub disabled: bool,
    /// Form name for native form submission. Stamped on the
    /// hidden `<input>` Pine emits alongside the radio buttons so
    /// non-JS form posts see the selected value. Optional — omit
    /// when the group's value is managed entirely in app state.
    pub name: String,
}

impl Default for PineRadioGroupRoot {
    fn default() -> Self {
        Self {
            value: String::new(),
            orientation: "horizontal".into(),
            disabled: false,
            name: String::new(),
        }
    }
}

#[handlers]
impl PineRadioGroupRoot {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            provide(ROOT_KEY, scope);
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

/// Single choice inside a RadioGroup. `role="radio"`, toggles
/// `data-state="checked|unchecked"` and `aria-checked` based on
/// whether the Root's `value` matches this Item's `value`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineRadioGroupItem.poco")]
pub struct PineRadioGroupItem {
    pub value: String,
    pub disabled: bool,
    /// Mirrored from Root's `value` — `true` when this Item is
    /// the currently-selected one. Watched via `watch_scope_field`
    /// so template bindings (`aria-checked`, `data-state`) stay in
    /// sync without an explicit signal plumbed through props.
    pub checked: bool,
    /// Cached for the template's `aria-checked` expression. Not a
    /// user-facing prop.
    pub group_value: String,
}

#[handlers]
impl PineRadioGroupItem {
    pub fn on_setup(&mut self) {
        // Seed initial state from Root and publish this Item's
        // scope so a nested Indicator mirrors `checked`.
        if let Some(root) = inject::<ScopeId>(ROOT_KEY) {
            if let Some(scope) = Scope::find(root) {
                let v = scope.state.borrow().get("value");
                self.group_value = v.as_string().unwrap_or_default();
                self.checked = self.group_value == self.value;
            }
        }
        if let Some(scope) = current_scope_id() {
            provide(CHECKED_OWNER_KEY, scope);
        }
    }

    pub fn on_ready(&self) {
        let Some(root) = inject::<ScopeId>(ROOT_KEY) else { return };
        let me = this::<Self>();
        watch_scope_field::<String, _>(root, "value", move |new, _| {
            let new_v = new.clone();
            me.update(|s| {
                s.group_value = new_v.clone();
                s.checked = s.group_value == s.value;
            });
        });
    }

    /// Click handler — wires the selected value into Root and
    /// emits `pp:update:model` from Root's element so
    /// `pp-model:value` on the Root tag picks it up.
    pub fn select(&mut self) {
        if self.disabled {
            return;
        }
        let Some(root) = inject::<ScopeId>(ROOT_KEY) else { return };
        let Some(scope) = Scope::find(root) else { return };
        if scope
            .state
            .borrow()
            .get("disabled")
            .as_bool()
            .unwrap_or(false)
        {
            return;
        }
        let new_value = self.value.clone();
        if let Some(rc) = scope.typed::<PineRadioGroupRoot>() {
            let handle = Handle::new(rc, root);
            handle.update(|g: &mut PineRadioGroupRoot| {
                g.value = new_value;
            });
        }
        // Emit from Root's element so the pp-model listener on the
        // <pine-radio-group-root> tag catches it — matches the
        // Dialog / AlertDialog pattern.
        if let Some(root_el) = pocopine_core::walker::find_element_for_scope(root) {
            pocopine::emit_from(&root_el, "pp:update:model", self.value.clone());
        }
    }
}

// ── Indicator ─────────────────────────────────────────────────────

/// Decorative element rendered only when its enclosing Item is the
/// checked one. Gated by `pp-show="checked"` on the template so the
/// element stays in the DOM for CSS transitions but is hidden when
/// unchecked. Authors supply whatever visual (a dot, a filled
/// circle, an icon) via the default slot.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineRadioGroupIndicator.poco")]
pub struct PineRadioGroupIndicator {
    pub checked: bool,
}

#[handlers]
impl PineRadioGroupIndicator {
    pub fn on_setup(&mut self) {
        // Read the parent Item's initial `checked` synchronously so
        // the first pp-show evaluation sees the right value.
        if let Some(owner) = inject::<ScopeId>(CHECKED_OWNER_KEY) {
            if let Some(scope) = Scope::find(owner) {
                let v = scope.state.borrow().get("checked");
                self.checked = v.as_bool().unwrap_or(false);
            }
        }
    }

    pub fn on_ready(&self) {
        let Some(owner) = inject::<ScopeId>(CHECKED_OWNER_KEY) else { return };
        let me = this::<Self>();
        watch_scope_field::<bool, _>(owner, "checked", move |&c, _| {
            me.update(|s| s.checked = c);
        });
    }
}
