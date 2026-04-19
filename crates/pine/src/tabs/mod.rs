//! `PineTabs` — tablist primitive (panels author-owned).
//!
//! Renders the `role="tablist"` with one `role="tab"` button per
//! `TabDef`. `pp-roving.horizontal` drives keyboard navigation;
//! click selects. The component emits `pp:update:model` with the
//! selected value so `pp-model="current"` on the tag binds the
//! current tab to parent state.
//!
//! Panels are authored outside the tab list — one `<div
//! role="tabpanel" pp-show="current == 'foo'">` per panel,
//! typically alongside `<pine-tabs>`. This keeps the surface
//! simple; Pine's v0 doesn't try to generate panels.
//!
//! ```html
//! <pine-tabs pp-model="current" :tabs="tabs"></pine-tabs>
//! <div role="tabpanel" pp-show="current == 'account'">Account</div>
//! <div role="tabpanel" pp-show="current == 'security'">Security</div>
//! ```

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::Element;

/// A single tab entry. `value` is the identifier used by
/// `:pp-show="current == value"` selectors on the author's
/// panels.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TabDef {
    pub value: String,
    pub label: String,
    pub disabled: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTabs.poco")]
pub struct PineTabs {
    pub tabs: Vec<TabDef>,
    /// Currently-selected tab value. Two-way bindable via
    /// `pp-model="current"` on the tag.
    pub value: String,
}

#[handlers]
impl PineTabs {
    /// Click delegation on the tablist — read `data-value` off the
    /// nearest button, update `value`, fire the model event.
    pub fn on_click(&mut self, ev: web_sys::Event) {
        let Some(t) = ev.target() else { return };
        let Ok(start) = t.dyn_into::<Element>() else { return };
        let Ok(Some(btn)) = start.closest("[data-value]") else { return };
        let Some(value) = btn.get_attribute("data-value") else { return };
        if btn.get_attribute("aria-disabled").as_deref() == Some("true")
            || btn.has_attribute("disabled")
        {
            return;
        }
        self.select(value);
    }

    /// Programmatic selection. Template handlers route here via
    /// `on_click`; external callers can also invoke directly.
    pub fn select(&mut self, value: String) {
        self.value = value.clone();
        pocopine::dispatch_event(
            "pp:update:model",
            &wasm_bindgen::JsValue::from_str(&value),
        );
    }
}
