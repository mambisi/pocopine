//! `<pine-select-*>` — listbox-based single-value selector
//! (Radix-style compound).
//!
//! Eight parts:
//!
//! - **Root** — owns `value`, `open`, `disabled`, `name`
//!   (hidden `<input>` for form submission), and a
//!   `HashMap<value, label>` registry that Items populate
//!   during `on_setup` so `Value` can render the
//!   currently-selected item's text without re-querying the DOM.
//! - **Trigger** — `<button role="combobox" aria-haspopup="listbox">`.
//!   Click toggles Root.open.
//! - **Value** — reads the label from Root's registry for the
//!   current value; falls back to its own slot (placeholder).
//! - **Portal** — teleports Content to `<body>` when open.
//! - **Content** — `<ul role="listbox">` + `pp-roving` (focus-
//!   based arrow nav). Anchored to Trigger. Click-outside +
//!   Escape close.
//! - **Item** — `<li role="option">`. Click / Enter / Space
//!   select (pp-roving doesn't activate — explicit handlers).
//! - **ItemIndicator** — renders only when its parent Item is
//!   selected.
//! - **Separator** — `<li role="separator">` divider.
//!
//! ```html
//! <pine-select-root pp-model:value="city" name="city">
//!   <pine-select-trigger>
//!     <pine-select-value>Choose a city…</pine-select-value>
//!     <span aria-hidden="true">▾</span>
//!   </pine-select-trigger>
//!   <pine-select-portal>
//!     <pine-select-content>
//!       <pine-select-item value="lagos">Lagos</pine-select-item>
//!       <pine-select-item value="accra">Accra</pine-select-item>
//!       <pine-select-separator></pine-select-separator>
//!       <pine-select-item value="nairobi">Nairobi</pine-select-item>
//!     </pine-select-content>
//!   </pine-select-portal>
//! </pine-select-root>
//! ```

use crate::compound;
use pocopine::prelude::*;
use pocopine::{
    current_scope_id, emit_model, focus, inject, inject_key, provide, refs, watch_scope_field,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

const SLUG: &str = "select";

inject_key!(ROOT: Handle<PineSelectRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSelectRoot.poco", role = "scope")]
pub struct PineSelectRoot {
    /// Open state. Two-way bindable via `pp-model:open="current"`.
    #[prop]
    pub open: bool,
    /// Selected value. Two-way bindable via `pp-model:value="current"`.
    #[prop]
    pub value: String,
    /// Initial value applied on mount if `value` is still blank —
    /// mirrors the native `<select>` `defaultValue` convention.
    #[prop]
    pub default_value: String,
    /// Form-field name. When set, Root renders a hidden
    /// `<input name value>` so the Select participates in native
    /// form submission.
    #[prop]
    pub name: String,
    #[prop]
    pub disabled: bool,
    /// id used by Trigger's `aria-controls` and Content's own
    /// `id`. Derived from the Root's scope id.
    pub listbox_id: String,
    /// Registry populated by each Item's `on_setup`:
    /// `value → rendered label text`. Value reads this to show
    /// the current selection without a DOM lookup. Skipped from
    /// serde round-trip because it's derived, not authored.
    #[serde(skip)]
    pub labels: HashMap<String, String>,
}

#[handlers]
impl PineSelectRoot {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            self.listbox_id = format!("pine-select-listbox-{}", scope.0);
        }
        if self.value.is_empty() && !self.default_value.is_empty() {
            self.value = self.default_value.clone();
        }
        provide(&ROOT, this::<Self>());
    }

    pub fn open_self(&mut self) {
        if !self.open && !self.disabled {
            self.open = true;
        }
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
        }
    }

    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        self.open = !self.open;
    }

    /// Called by each Item during its own `on_setup` so Value
    /// can show the active label without scanning the DOM.
    pub fn register_label(&mut self, value: String, label: String) {
        self.labels.insert(value, label);
    }

    pub fn select_value(&mut self, value: String) {
        if self.value != value {
            self.value = value.clone();
            emit_model(value);
        }
        self.open = false;
    }
}

// ── Trigger ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSelectTrigger.poco", role = "interactive")]
pub struct PineSelectTrigger {
    #[observe(ROOT)] pub open: bool,
    #[observe(ROOT)] pub disabled: bool,
    #[observe(ROOT)] pub value: String,
    #[observe(ROOT)] pub listbox_id: String,
}

#[handlers]
impl PineSelectTrigger {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = inject::<Handle<PineSelectRoot>>(&ROOT) else {
            return;
        };
        // Stamp the trigger with its root scope id so Content
        // can anchor to it without the author coordinating a
        // selector.
        if let Some(btn) = refs.get("trigger") {
            compound::stamp_trigger(&btn, root.scope_id(), SLUG);
        }
    }

    pub fn toggle(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineSelectRoot| r.toggle());
        }
    }
}

// ── Value ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSelectValue.poco", role = "visual")]
pub struct PineSelectValue {
    pub display_label: String,
    pub has_value: bool,
}

#[handlers]
impl PineSelectValue {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.with(|r| {
                self.has_value = !r.value.is_empty();
                self.display_label = r
                    .labels
                    .get(&r.value)
                    .cloned()
                    .unwrap_or_else(|| r.value.clone());
            });
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject::<Handle<PineSelectRoot>>(&ROOT) else {
            return;
        };
        let root_scope = root.scope_id();
        let root_for_watch = root;

        // Re-pull the label whenever the current value changes.
        watch_scope_field::<String, _>(root_scope, "value", move |v, _| {
            let current = v.clone();
            let label = root_for_watch
                .with(|r| r.labels.get(&current).cloned())
                .unwrap_or_else(|| current.clone());
            let has_value = !current.is_empty();
            handle.update(|s| {
                s.has_value = has_value;
                s.display_label = label;
            });
        });
    }
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSelectPortal.poco", role = "scope")]
pub struct PineSelectPortal {
    #[observe(ROOT)] pub open: bool,
}

#[handlers]
impl PineSelectPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineSelectContent.poco", role = "list", transition = "fade")]
pub struct PineSelectContent {
    pub anchor: String,
    pub listbox_id: String,
    #[prop]
    pub side: String,
    #[prop]
    pub align: String,
    #[prop]
    pub side_offset: f64,
}

impl Default for PineSelectContent {
    fn default() -> Self {
        Self {
            anchor: String::new(),
            listbox_id: String::new(),
            side: "bottom".into(),
            align: "start".into(),
            side_offset: 4.0,
        }
    }
}

#[handlers]
impl PineSelectContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PineSelectRoot>>(&ROOT) {
            let (scope, lb) = root.with(|r| (root.scope_id(), r.listbox_id.clone()));
            self.anchor = compound::trigger_selector(scope, SLUG);
            self.listbox_id = lb;
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(menu) = refs.get("menu") else { return };
        // Exempt our own trigger from `@click.outside` so the
        // trigger-while-open click closes cleanly instead of
        // racing outside-close (capture) + trigger-toggle (bubble).
        if let Some(root) = inject::<Handle<PineSelectRoot>>(&ROOT) {
            let _ = menu.set_attribute(
                "data-pp-outside-exempt",
                &compound::trigger_selector(root.scope_id(), SLUG),
            );
        }
        // Focus the selected item (data-state="checked") if one
        // exists, otherwise the first enabled option.
        let initial = menu
            .query_selector("[role=\"option\"][data-state=\"checked\"]:not([data-disabled=\"true\"])")
            .ok()
            .flatten()
            .or_else(|| {
                menu.query_selector("[role=\"option\"]:not([data-disabled=\"true\"])")
                    .ok()
                    .flatten()
            });
        if let Some(item) = initial {
            // Roving expects exactly one tabindex=0 per group.
            if let Ok(list) = menu.query_selector_all("[role=\"option\"]") {
                for i in 0..list.length() {
                    if let Some(node) = list.item(i) {
                        if let Ok(el) = node.dyn_into::<web_sys::Element>() {
                            let _ = el.set_attribute("tabindex", "-1");
                        }
                    }
                }
            }
            let _ = item.set_attribute("tabindex", "0");
            if let Ok(html) = item.dyn_into::<HtmlElement>() {
                // Plain `focus()` scrolls the focused element into
                // view — and when the Select's popover is teleported
                // to <body>, that "into view" computation can pull
                // the whole page to wherever the popover landed in
                // body flow. Use the `preventScroll: true` variant
                // so keyboard focus moves without yanking the page.
                focus::focus_no_scroll(&html);
            }
        } else {
            focus::auto_focus_first(&menu);
        }

        // Programmatic anchor install — same pattern as Popover
        // + DropdownMenu so author-authored placement / offset
        // flow through.
        if let Ok(floater) = menu.dyn_into::<HtmlElement>() {
            if let Some(root) = inject::<Handle<PineSelectRoot>>(&ROOT) {
                compound::install_anchor_to_trigger(
                    &floater,
                    root.scope_id(),
                    SLUG,
                    &self.side,
                    &self.align,
                    self.side_offset,
                    true,
                );
            }
        }
    }

    pub fn close(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineSelectRoot| r.close());
            schedule_trigger_focus(root.scope_id());
        }
    }
}

/// Move focus back to the Trigger after close. Deferred so the
/// reactive flush finishes before `.focus()`.
fn schedule_trigger_focus(root_scope: pocopine::ScopeId) {
    pocopine::tick::after_flush(move || {
        let Some(trigger) = refs::get_on(root_scope, "trigger") else {
            return;
        };
        // `trigger` ref lives on Trigger's template root, which
        // is inside Trigger's scope. Root's scope holds the
        // container only. So look up Trigger by its data-*
        // stamp instead.
        let _ = trigger;
        if let Some(el) = compound::resolve_trigger(root_scope, SLUG) {
            if let Ok(html) = el.dyn_into::<HtmlElement>() {
                // No-scroll focus — after closing the dropdown
                // we don't want the page to snap to wherever the
                // trigger happens to sit.
                focus::focus_no_scroll(&html);
            }
        }
    });
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSelectItem.poco", role = "item")]
pub struct PineSelectItem {
    #[prop]
    pub value: String,
    #[prop]
    pub disabled: bool,
    /// Mirrored from Root.value — `true` when this Item's value
    /// is the current selection.
    pub selected: bool,
}

// Provide key for ItemIndicator's `selected` mirror.
inject_key!(SELECTED_OWNER: pocopine::ScopeId);

#[handlers]
impl PineSelectItem {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.selected = root.with(|r| r.value == self.value);
        }
        // Provide the Item's own scope id so a nested
        // ItemIndicator can mirror `selected` without having to
        // go back to Root.
        if let Some(scope) = current_scope_id() {
            provide(&SELECTED_OWNER, scope);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        // Register the item's label with Root so Value can show
        // the rendered text.
        let label = refs
            .get("item")
            .and_then(|el| el.text_content())
            .unwrap_or_default()
            .trim()
            .to_string();
        if let Some(root) = inject::<Handle<PineSelectRoot>>(&ROOT) {
            let value = self.value.clone();
            root.update(|r: &mut PineSelectRoot| r.register_label(value, label));
        }

        let Some(root) = inject::<Handle<PineSelectRoot>>(&ROOT) else {
            return;
        };
        let my_value = self.value.clone();
        let root_scope = root.scope_id();
        let h = handle;
        watch_scope_field::<String, _>(root_scope, "value", move |v, _| {
            let is_selected = v == &my_value;
            h.update(|s| s.selected = is_selected);
        });
    }

    pub fn select(&mut self) {
        if self.disabled {
            return;
        }
        let my_value = self.value.clone();
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineSelectRoot| r.select_value(my_value));
            schedule_trigger_focus(root.scope_id());
        }
    }
}

// ── ItemIndicator ─────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSelectItemIndicator.poco", role = "visual")]
pub struct PineSelectItemIndicator {
    pub selected: bool,
}

#[handlers]
impl PineSelectItemIndicator {
    pub fn on_setup(&mut self) {
        if let Some(owner) = inject(&SELECTED_OWNER) {
            if let Some(scope) = pocopine::Scope::find(owner) {
                let v = scope.state.borrow().get("selected");
                self.selected = v.as_bool().unwrap_or(false);
            }
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(owner) = inject(&SELECTED_OWNER) else {
            return;
        };
        watch_scope_field::<bool, _>(owner, "selected", move |&v, _| {
            handle.update(|s| s.selected = v);
        });
    }
}

// ── Separator ─────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineSelectSeparator.poco", role = "item")]
pub struct PineSelectSeparator {}

#[handlers]
impl PineSelectSeparator {}
