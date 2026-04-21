//! `<pine-accordion-*>` — expand/collapse sections primitive.
//!
//! Compound after reka-ui's Accordion. Root owns the open-value
//! state, each Item scopes its own `value` + mirrored `open`,
//! and Trigger / Content inject the Item to interact with its
//! slot of state.
//!
//! Two modes, chosen per tag:
//!
//! - `type="single"` (default, with `collapsible="true"`) —
//!   `value: String` holds the currently-open item id, `""` for
//!   all closed. Bind with `pp-model:value`.
//! - `type="multiple"` — `values: Vec<String>` holds every open
//!   item. Bind with `pp-model:values`.
//!
//! ```html
//! <pine-accordion-root type="single" collapsible>
//!   <pine-accordion-item value="a">
//!     <pine-accordion-trigger>A</pine-accordion-trigger>
//!     <pine-accordion-content>Body A</pine-accordion-content>
//!   </pine-accordion-item>
//!   <pine-accordion-item value="b">
//!     <pine-accordion-trigger>B</pine-accordion-trigger>
//!     <pine-accordion-content>Body B</pine-accordion-content>
//!   </pine-accordion-item>
//! </pine-accordion-root>
//! ```

use pocopine::prelude::*;
use pocopine::{current_scope_id, inject, inject_key, provide, watch_scope_field};
use serde::{Deserialize, Serialize};

inject_key!(ROOT: Handle<PineAccordionRoot>);
inject_key!(ITEM: ScopeId);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineAccordionRoot.poco", role = "panel")]
pub struct PineAccordionRoot {
    /// `"single"` (default) or `"multiple"`.
    #[prop] pub r#type: String,
    /// When type="single" and collapsible=true, clicking the open
    /// item collapses it. When collapsible=false, at least one
    /// item must stay open (clicking the open item is a no-op).
    #[prop] pub collapsible: bool,
    /// `type="single"` state. `""` when all closed.
    #[prop] pub value: String,
    /// `type="multiple"` state.
    #[prop] pub values: Vec<String>,
}

impl Default for PineAccordionRoot {
    fn default() -> Self {
        Self {
            r#type: "single".into(),
            collapsible: true,
            value: String::new(),
            values: Vec::new(),
        }
    }
}

#[handlers]
impl PineAccordionRoot {
    pub fn on_setup(&mut self) {
        provide(&ROOT, this::<Self>());
    }

    /// Invoked by PineAccordionItem when its Trigger is clicked.
    pub fn toggle_item(&mut self, item_value: String) {
        match self.r#type.as_str() {
            "multiple" => {
                if let Some(i) = self.values.iter().position(|v| v == &item_value) {
                    self.values.remove(i);
                } else {
                    self.values.push(item_value);
                }
                // pp-model listens on `pp:update:model` only —
                // see the matching note in tabs::emit_from_self.
                emit("pp:update:model", self.values.clone());
            }
            _ => {
                // single
                if self.value == item_value {
                    if self.collapsible {
                        self.value = String::new();
                    }
                    // non-collapsible: click on open → no-op
                } else {
                    self.value = item_value;
                }
                emit("pp:update:model", self.value.clone());
            }
        }
    }
}

/// Helpers kept outside the `#[handlers]` block so they're not
/// exposed as template-dispatchable handler names.
impl PineAccordionRoot {
    /// Membership check an Item calls during its `on_setup` /
    /// mirror to decide whether it's currently open. Branches on
    /// `type`.
    pub fn item_open(&self, item_value: &str) -> bool {
        match self.r#type.as_str() {
            "multiple" => self.values.iter().any(|v| v == item_value),
            _ => self.value == item_value,
        }
    }
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAccordionItem.poco", role = "panel")]
pub struct PineAccordionItem {
    /// Stable identifier for this item within the Root.
    #[prop] pub value: String,
    #[prop] pub disabled: bool,
    /// Mirrored from Root's state via `item_open(self.value)`.
    pub open: bool,
}

#[handlers]
impl PineAccordionItem {
    pub fn on_setup(&mut self) {
        // Seed initial `open` from the root's current state so
        // Trigger / Content see the right value on first bind.
        if let Some(root) = inject(&ROOT) {
            let value = self.value.clone();
            self.open = root.with(|r| r.item_open(&value));
        }
        // Provide this Item's scope id + value to Trigger /
        // Content so they don't need to know Root's type shape.
        if let Some(scope) = current_scope_id() {
            provide(&ITEM, scope);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject(&ROOT) else {
            return;
        };
        let my_value = self.with_value();
        let root_scope = root.scope_id();
        // Watch the single-mode `value` …
        let my_value_1 = my_value.clone();
        let me_1 = handle.clone();
        let root_1 = root.clone();
        watch_scope_field::<String, _>(root_scope, "value", move |_, _| {
            let is_open = root_1.with(|r| r.item_open(&my_value_1));
            me_1.update(|s| s.open = is_open);
        });
        // … and the multi-mode `values`. Exactly one will apply
        // based on root.type; watching both keeps the code
        // simple and paying-for-what-you-use is negligible.
        let my_value_2 = my_value;
        let me_2 = handle;
        let root_2 = root;
        watch_scope_field::<Vec<String>, _>(root_scope, "values", move |_, _| {
            let is_open = root_2.with(|r| r.item_open(&my_value_2));
            me_2.update(|s| s.open = is_open);
        });
    }

    pub fn click_trigger(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            let v = self.value.clone();
            root.update(|r: &mut PineAccordionRoot| r.toggle_item(v));
        }
    }
}

impl PineAccordionItem {
    fn with_value(&self) -> String {
        self.value.clone()
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAccordionTrigger.poco", role = "interactive")]
pub struct PineAccordionTrigger {
    pub open: bool,
    pub disabled: bool,
}

#[handlers]
impl PineAccordionTrigger {
    pub fn on_setup(&mut self) {
        if let Some(item) = inject(&ITEM) {
            if let Some(scope) = Scope::find(item) {
                let s = scope.state.borrow();
                self.open = s.get("open").as_bool().unwrap_or(false);
                self.disabled = s.get("disabled").as_bool().unwrap_or(false);
            }
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(item) = inject(&ITEM) else { return };
        watch_scope_field::<bool, _>(item, "open", move |&is_open, _| {
            handle.update(|s| s.open = is_open);
        });
    }

    pub fn click(&mut self) {
        let Some(item) = inject(&ITEM) else { return };
        if let Some(scope) = Scope::find(item) {
            if let Some(inner) = scope.typed::<PineAccordionItem>() {
                Handle::new(inner, item).update(|s: &mut PineAccordionItem| s.click_trigger());
            }
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
// `transition` lives on the CLONE root inside the .poco template
// (see PineAccordionContent.poco) — pp-if's clone is outside the
// macro-stamp scope, so the macro arg won't reach it.
#[component(template = "PineAccordionContent.poco", role = "panel")]
pub struct PineAccordionContent {
    pub open: bool,
}

#[handlers]
impl PineAccordionContent {
    pub fn on_setup(&mut self) {
        if let Some(item) = inject(&ITEM) {
            if let Some(scope) = Scope::find(item) {
                self.open = scope
                    .state
                    .borrow()
                    .get("open")
                    .as_bool()
                    .unwrap_or(false);
            }
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(item) = inject(&ITEM) else { return };
        watch_scope_field::<bool, _>(item, "open", move |&is_open, _| {
            handle.update(|s| s.open = is_open);
        });
    }
}
