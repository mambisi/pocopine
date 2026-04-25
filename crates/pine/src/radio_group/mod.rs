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
use pocopine::{create_context, current_scope_id, watch_scope_field_scoped};
use pocopine_core::reactive::ScopeId;
use pocopine_core::scope::Scope;
use serde::{Deserialize, Serialize};

create_context!(ROOT: ScopeId);
// Per-Item scope publication — a nested Indicator injects it to
// mirror its owner's `checked`. Each compound that ships an
// indicator pattern declares its own key so compounds stay
// isolated (DropdownMenu has its own CHECKED_OWNER).
create_context!(CHECKED_OWNER: ScopeId);

// ── Root ──────────────────────────────────────────────────────────

/// Radio group container. Owns `value`; provides its scope id so
/// Items can read + write it through `Scope::find` + Handle::update.
#[derive(Serialize, Deserialize)]
#[component(template = "PineRadioGroupRoot.poco", role = "panel")]
// RFC 049 — only `<pine-radio-group-item>` is a semantically
// meaningful direct child of the group. Items own the `role="radio"`
// contract and keyboard navigation; anything else here either breaks
// roving-tabindex or confuses screen readers about group size.
#[slot(default, only = [PineRadioGroupItem])]
pub struct PineRadioGroupRoot {
    #[model]
    pub value: String,
    /// `"horizontal"` (default) or `"vertical"`. Drives both the
    /// `data-orientation` attribute and the roving direction —
    /// horizontal uses Arrow-Left / Arrow-Right, vertical uses
    /// Arrow-Up / Arrow-Down.
    #[prop]
    pub orientation: String,
    /// Disables every Item when true. Individual Items can also be
    /// disabled via their own `disabled` prop.
    #[prop]
    pub disabled: bool,
    /// Form name for native form submission. Stamped on the
    /// hidden `<input>` Pine emits alongside the radio buttons so
    /// non-JS form posts see the selected value. Optional — omit
    /// when the group's value is managed entirely in app state.
    #[prop]
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
            ROOT.provide(scope);
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

/// Single choice inside a RadioGroup. `role="radio"`, toggles
/// `data-state="checked|unchecked"` and `aria-checked` based on
/// whether the Root's `value` matches this Item's `value`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineRadioGroupItem.poco", role = "interactive")]
// RFC 049 — RadioGroupItem commonly holds an Indicator plus
// label text. `accepts` (blanket) so the Item is permissive
// for arbitrary author content while still declaring the
// Indicator as its recognised semantic part.
#[slot(default, accepts = [PineRadioGroupIndicator])]
pub struct PineRadioGroupItem {
    #[prop]
    pub value: String,
    #[prop]
    pub disabled: bool,
    /// Mirrored from Root's `value` — `true` when this Item is
    /// the currently-selected one. Watched via `watch_scope_field_scoped`
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
        if let Some(root) = ROOT.inject() {
            if let Some(scope) = Scope::find(root) {
                let v = scope.state.borrow().get("value");
                self.group_value = v.as_string().unwrap_or_default();
                self.checked = self.group_value == self.value;
            }
        }
        if let Some(scope) = current_scope_id() {
            CHECKED_OWNER.provide(scope);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = ROOT.inject() else { return };
        watch_scope_field_scoped::<String, _>(root, "value", move |new, _| {
            let new_v = new.clone();
            handle.update(|s| {
                s.group_value = new_v.clone();
                s.checked = s.group_value == s.value;
            });
        });
    }

    /// Click handler — wires the selected value into Root.
    pub fn select(&mut self) {
        if self.disabled {
            return;
        }
        let Some(root) = ROOT.inject() else { return };
        let Some(scope) = Scope::find(root) else {
            return;
        };
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
    }
}

// ── Indicator ─────────────────────────────────────────────────────

/// Decorative element rendered only when its enclosing Item is the
/// checked one. Gated by `pp-show="checked"` on the template so the
/// element stays in the DOM for CSS transitions but is hidden when
/// unchecked. Authors supply whatever visual (a dot, a filled
/// circle, an icon) via the default slot.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineRadioGroupIndicator.poco", role = "visual")]
pub struct PineRadioGroupIndicator {
    pub checked: bool,
}

#[handlers]
impl PineRadioGroupIndicator {
    pub fn on_setup(&mut self) {
        // Read the parent Item's initial `checked` synchronously so
        // the first pp-show evaluation sees the right value.
        if let Some(owner) = CHECKED_OWNER.inject() {
            if let Some(scope) = Scope::find(owner) {
                let v = scope.state.borrow().get("checked");
                self.checked = v.as_bool().unwrap_or(false);
            }
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(owner) = CHECKED_OWNER.inject() else {
            return;
        };
        watch_scope_field_scoped::<bool, _>(owner, "checked", move |&c, _| {
            handle.update(|s| s.checked = c);
        });
    }
}
