//! `<pine-tabs-*>` — tabs primitive (compound).
//!
//! Reka-ui / Radix anatomy. Four parts — Indicator skipped in
//! v0; authors style `[data-state="active"]` on Trigger for the
//! highlight strip.
//!
//! - **Root** (`pine-tabs-root`) — owns `value: String` (currently
//!   selected tab), `orientation` (`horizontal` / `vertical`).
//! - **List** (`pine-tabs-list`) — `role="tablist"` wrapper,
//!   `pp-roving` for arrow-key navigation.
//! - **Trigger** (`pine-tabs-trigger`) — `role="tab"` button, has
//!   its own `value` prop. Clicking sets Root.value. ARIA id
//!   derived from Root scope + value so Content can target it.
//! - **Content** (`pine-tabs-content`) — `role="tabpanel"`,
//!   `pp-show`-gated on matching Root.value. `aria-labelledby`
//!   points at the matching Trigger.
//!
//! ```html
//! <pine-tabs-root pp-model:value="current">
//!   <pine-tabs-list>
//!     <pine-tabs-trigger value="a">Account</pine-tabs-trigger>
//!     <pine-tabs-trigger value="b">Billing</pine-tabs-trigger>
//!   </pine-tabs-list>
//!   <pine-tabs-content value="a">…</pine-tabs-content>
//!   <pine-tabs-content value="b">…</pine-tabs-content>
//! </pine-tabs-root>
//! ```

use pocopine::prelude::*;
use pocopine::{inject, inject_key, provide, watch_scope_field};
use serde::{Deserialize, Serialize};

inject_key!(ROOT: Handle<PineTabsRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineTabsRoot.poco", role = "panel")]
pub struct PineTabsRoot {
    #[model] pub value: String,
    /// `"horizontal"` (default) or `"vertical"`. Flows into
    /// `aria-orientation` on List and dictates the roving
    /// nav-axis.
    #[prop] pub orientation: String,
}

impl Default for PineTabsRoot {
    fn default() -> Self {
        Self {
            value: String::new(),
            orientation: "horizontal".into(),
        }
    }
}

#[handlers]
impl PineTabsRoot {
    pub fn on_setup(&mut self) {
        provide(&ROOT, this::<Self>());
    }

    pub fn select(&mut self, value: String) {
        if self.value != value {
            self.value = value;
        }
    }
}

// ── List ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTabsList.poco", role = "panel")]
pub struct PineTabsList {
    #[observe(ROOT)] pub orientation: String,
}

#[handlers]
impl PineTabsList {}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTabsTrigger.poco", role = "interactive")]
pub struct PineTabsTrigger {
    /// Author-set id of the tab this trigger activates.
    #[prop] pub value: String,
    #[prop] pub disabled: bool,
    /// Mirrored — `true` when Root.value == self.value.
    pub selected: bool,
    /// ARIA id derived from Root's scope id + value, so Content
    /// can set `aria-labelledby` to the same string.
    pub trigger_id: String,
}

#[handlers]
impl PineTabsTrigger {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.trigger_id = format!(
                "pine-tabs-trigger-{}-{}",
                root.scope_id().0,
                self.value
            );
            self.selected = root.with(|r| r.value == self.value);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject(&ROOT) else { return };
        let my_value = self.value.clone();
        let root_scope = root.scope_id();
        watch_scope_field::<String, _>(root_scope, "value", move |new, _| {
            let is_selected = new == &my_value;
            handle.update(|s| s.selected = is_selected);
        });
    }

    pub fn click(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            let v = self.value.clone();
            root.update(|r: &mut PineTabsRoot| r.select(v));
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTabsContent.poco", role = "panel")]
pub struct PineTabsContent {
    #[prop] pub value: String,
    /// Mirrored.
    pub selected: bool,
    /// `aria-labelledby` target — the matching Trigger's id.
    pub trigger_id: String,
}

#[handlers]
impl PineTabsContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.trigger_id = format!(
                "pine-tabs-trigger-{}-{}",
                root.scope_id().0,
                self.value
            );
            self.selected = root.with(|r| r.value == self.value);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject(&ROOT) else { return };
        let my_value = self.value.clone();
        let root_scope = root.scope_id();
        watch_scope_field::<String, _>(root_scope, "value", move |new, _| {
            let is_selected = new == &my_value;
            handle.update(|s| s.selected = is_selected);
        });
    }
}
