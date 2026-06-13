//! `<pine-toggle-group-*>` — set of Toggles with a shared selection
//! policy.
//!
//! Two selection modes:
//!
//! - `type="single"` — exactly one Item may be pressed at a time
//!   (like Tabs but with a pressed-looking button). Optional —
//!   if `rovable=true`, clicking the pressed item unpresses it.
//!   Bind via `pp-model:value="current"`; `""` means "nothing
//!   pressed".
//! - `type="multiple"` — any subset may be pressed. Bind via
//!   `pp-model:values="current"` where `current` is a
//!   `Vec<String>`.
//!
//! Arrow-key navigation via `pp-roving.both` on Root — Up/Down/
//! Left/Right all rove between items, matching reka-ui's default.
//! Space/Enter press/release the focused item through the native
//! button's click.
//!
//! ```html
//! <pine-toggle-group-root type="single" pp-model:value="align">
//!   <pine-toggle-group-item value="left">Left</pine-toggle-group-item>
//!   <pine-toggle-group-item value="center">Center</pine-toggle-group-item>
//!   <pine-toggle-group-item value="right">Right</pine-toggle-group-item>
//! </pine-toggle-group-root>
//! ```

use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, watch_scope_field_scoped};
use pocopine_core::reactive::ScopeId;
use pocopine_core::scope::Scope;
use serde::{Deserialize, Serialize};

create_context!(ROOT: ScopeId);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineToggleGroupRoot.poco", role = "panel")]
// RFC 049 — same shape as RadioGroup: a single-kind container
// of Items with roving tabindex. Foreign elements would break
// keyboard navigation between the toggles.
#[slot(default, only = [PineToggleGroupItem])]
pub struct PineToggleGroupRoot {
    /// `"single"` (default) or `"multiple"`.
    #[prop]
    pub r#type: String,
    /// Single-mode current value. `""` when nothing is pressed.
    #[model]
    pub value: String,
    /// Multiple-mode current values.
    #[model]
    pub values: Vec<String>,
    /// `"horizontal"` (default) / `"vertical"` — flows to
    /// `data-orientation` for styling. Roving is `.both` either
    /// way so keyboard nav works without orientation-specific
    /// configuration.
    #[prop]
    pub orientation: String,
    #[prop]
    pub disabled: bool,
}

impl Default for PineToggleGroupRoot {
    fn default() -> Self {
        Self {
            r#type: "single".into(),
            value: String::new(),
            values: Vec::new(),
            orientation: "horizontal".into(),
            disabled: false,
        }
    }
}

#[handlers]
impl PineToggleGroupRoot {
    fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            ROOT.provide(scope);
        }
    }

    /// Invoked by Item on click. Branches on `type`.
    pub fn toggle_item(&mut self, item_value: String) {
        match self.r#type.as_str() {
            "multiple" => {
                if let Some(i) = self.values.iter().position(|v| v == &item_value) {
                    self.values.remove(i);
                } else {
                    self.values.push(item_value);
                }
            }
            _ => {
                // single — click on the currently-pressed item
                // clears the selection (matches reka-ui default).
                if self.value == item_value {
                    self.value = String::new();
                } else {
                    self.value = item_value;
                }
            }
        }
    }
}

/// Helpers outside `#[handlers]` so they're not exposed as template
/// dispatch targets.
impl PineToggleGroupRoot {
    pub fn item_pressed(&self, item_value: &str) -> bool {
        match self.r#type.as_str() {
            "multiple" => self.values.iter().any(|v| v == item_value),
            _ => self.value == item_value,
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

/// Single button inside a ToggleGroup. Same rendered shape as
/// standalone `<pine-toggle>` (so authors can theme one class +
/// share CSS) — distinguished only by its membership in the
/// group's roving + selection policy.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineToggleGroupItem.poco", role = "interactive")]
pub struct PineToggleGroupItem {
    #[prop]
    pub value: String,
    #[prop]
    pub disabled: bool,
    /// Mirrored from Root's selection. Watched via
    /// `watch_scope_field_scoped` on both `value` and `values`; exactly
    /// one applies per tick based on Root's `type`.
    pub pressed: bool,
}

#[handlers]
impl PineToggleGroupItem {
    fn on_setup(&mut self) {
        if let Some(root) = ROOT.inject()
            && let Some(scope) = Scope::find(root)
                && let Some(rc) = scope.typed::<PineToggleGroupRoot>() {
                    self.pressed = rc.borrow().item_pressed(&self.value);
                }
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = ROOT.inject() else { return };
        let my_value = self.value.clone();
        let root_s = root;
        // Single-mode watcher — Root.value.
        let me1 = handle.clone();
        let my_value1 = my_value.clone();
        watch_scope_field_scoped::<String, _>(root_s, "value", move |_, _| {
            let is_pressed = Scope::find(root_s)
                .and_then(|s| s.typed::<PineToggleGroupRoot>())
                .map(|rc| rc.borrow().item_pressed(&my_value1))
                .unwrap_or(false);
            me1.update(|s| s.pressed = is_pressed);
        });
        // Multiple-mode watcher — Root.values.
        let me2 = handle;
        let my_value2 = my_value;
        watch_scope_field_scoped::<Vec<String>, _>(root_s, "values", move |_, _| {
            let is_pressed = Scope::find(root_s)
                .and_then(|s| s.typed::<PineToggleGroupRoot>())
                .map(|rc| rc.borrow().item_pressed(&my_value2))
                .unwrap_or(false);
            me2.update(|s| s.pressed = is_pressed);
        });
    }

    pub fn click(&mut self) {
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
        if let Some(rc) = scope.typed::<PineToggleGroupRoot>() {
            let my_value = self.value.clone();
            Handle::new(rc, root).update(|g: &mut PineToggleGroupRoot| g.toggle_item(my_value));
        }
    }
}
