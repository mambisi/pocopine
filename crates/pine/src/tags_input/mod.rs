//! `<pine-tags-input-*>` — chip-style tag entry (compound).
//!
//! Six parts, matching Radix-Vue's TagsInput anatomy:
//!
//! - **Root** (`pine-tags-input-root`) — owns `values: Vec<String>`,
//!   enforces `duplicate` + `max` constraints, provides its Handle.
//!   `pp-model:values="my_tags"` round-trips the list with the
//!   parent via `pp:update:model` emits on every mutation.
//! - **Item** (`pine-tags-input-item`) — one chip. Author iterates
//!   with `pp-for` and passes the chip's value through `:value`;
//!   Item provides that value so nested ItemText / ItemDelete can
//!   find it without the author wiring any keys.
//! - **ItemText** (`pine-tags-input-item-text`) — renders the
//!   enclosing Item's value as text. Zero author ergonomics.
//! - **ItemDelete** (`pine-tags-input-item-delete`) — `<button>`
//!   that removes its enclosing Item from Root.values on click.
//! - **Input** (`pine-tags-input-input`) — `<input>` wrapped in a
//!   `<div>`. Enter commits the current field value as a new tag;
//!   Backspace-while-empty pops the last tag.
//! - **Clear** (`pine-tags-input-clear`) — `<button>` that empties
//!   Root.values on click.
//!
//! ```html
//! <pine-tags-input-root pp-model:values="tags">
//!   <pine-tags-input-item pp-for="tag in tags" pp-key="tag" :value="tag">
//!     <pine-tags-input-item-text></pine-tags-input-item-text>
//!     <pine-tags-input-item-delete aria-label="Remove">×</pine-tags-input-item-delete>
//!   </pine-tags-input-item>
//!   <pine-tags-input-input placeholder="Add tag…"></pine-tags-input-input>
//!   <pine-tags-input-clear>Clear</pine-tags-input-clear>
//! </pine-tags-input-root>
//! ```
//!
//! Root props:
//! - `values: Vec<String>` — the tags. `#[prop]`, two-way-bindable.
//! - `duplicate: bool` — allow repeat values. Default `false`
//!   (duplicates silently rejected so parents don't have to dedupe).
//! - `max: u32` — cap on tag count. `0` = unlimited (the default).
//! - `disabled: bool` — observed by Input/Clear/Item to gate input.

use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, refs};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

create_context!(ROOT: Handle<PineTagsInputRoot>);
// `ITEM` carries the enclosing Item's ScopeId — not its value
// string — because `:value="tag"` on the Item tag is a reactive
// binding, so the Item's `self.value` is "" when its on_setup
// runs and only flips to the real string after a subsequent
// effect. Carrying the scope id lets ItemText / ItemDelete read
// the Item's value *lazily* (or watch it) from the authoritative
// scope, sidestepping the setup-order hazard.
create_context!(ITEM: ScopeId);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineTagsInputRoot.poco",
    role = "scope",
    display = "contents"
)]
// RFC 049 — TagsInput Root holds Items (the tag pills) + one
// Input (for typing new tags) + an optional Clear button.
// `accepts` because authors often wrap items in containers or
// add separator labels.
#[slot(default, accepts = [
    PineTagsInputItem,
    PineTagsInputInput,
    PineTagsInputClear,
])]
pub struct PineTagsInputRoot {
    /// The list of tags. Bind with `pp-model:values="my_tags"`.
    #[model]
    pub values: Vec<String>,
    /// When `false` (default), `add_tag` silently drops values
    /// that already exist in the list.
    #[prop]
    pub duplicate: bool,
    /// Maximum tag count. `0` = unlimited.
    #[prop]
    pub max: u32,
    /// Mirrored onto Input / Clear / Item via `#[observe(ROOT)]`
    /// to gate interactions.
    #[prop]
    pub disabled: bool,
}

#[handlers]
impl PineTagsInputRoot {
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
    }

    /// Add a tag. Trimmed; empty strings drop. Duplicates drop
    /// unless `duplicate=true`. Refused when `max` is reached.
    pub fn add_tag(&mut self, v: String) {
        if self.disabled {
            return;
        }
        let trimmed = v.trim();
        if trimmed.is_empty() {
            return;
        }
        if !self.duplicate && self.values.iter().any(|x| x == trimmed) {
            return;
        }
        if self.max > 0 && self.values.len() as u32 >= self.max {
            return;
        }
        self.values.push(trimmed.to_string());
    }

    /// Remove the first tag whose value equals `v`.
    pub fn remove_value(&mut self, v: String) {
        if let Some(i) = self.values.iter().position(|x| x == &v) {
            self.values.remove(i);
        }
    }

    /// Remove the last tag, if any. Called by Input on
    /// Backspace-while-empty.
    pub fn remove_last(&mut self) {
        if self.values.pop().is_some() {}
    }

    pub fn clear(&mut self) {
        if self.values.is_empty() {
            return;
        }
        self.values.clear();
    }
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineTagsInputItem.poco",
    role = "panel",
    display = "contents",
    animate = "flip",
    transition = "scale"
)]
// RFC 049 — each tag pill holds its ItemText (label) + an
// ItemDelete (the × button). Authors may drop icons or
// additional markup, so `accepts` rather than `only`.
#[slot(default, accepts = [PineTagsInputItemText, PineTagsInputItemDelete])]
pub struct PineTagsInputItem {
    /// The tag value this Item represents. Passed via
    /// `:value="tag"` from the author's `pp-for`.
    #[prop]
    pub value: String,
    /// Mirrored from Root.
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineTagsInputItem {
    fn on_setup(&mut self) {
        // Publish our scope id so nested ItemText / ItemDelete
        // can resolve the value live from our state, not from a
        // snapshot taken before reactive `:value` bindings have
        // fired.
        if let Some(scope) = current_scope_id() {
            ITEM.provide(scope);
        }
    }
}

// ── ItemText ──────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineTagsInputItemText.poco",
    role = "visual",
    display = "contents"
)]
pub struct PineTagsInputItemText {
    /// Rendered label. Seeded in `on_setup` from the injected
    /// ITEM value; never mutated after mount since a tag's value
    /// is stable for its Item's lifetime.
    pub text: String,
}

#[handlers]
impl PineTagsInputItemText {
    fn on_setup(&mut self) {
        // Seed from whatever the Item's scope currently has —
        // may still be "" if the reactive `:value` binding
        // hasn't fired yet; the `on_ready` watch covers that.
        if let Some(item_scope) = ITEM.inject() {
            if let Some(scope) = Scope::find(item_scope) {
                self.text = scope
                    .state
                    .borrow()
                    .get("value")
                    .as_string()
                    .unwrap_or_default();
            }
        }
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(item_scope) = ITEM.inject() else {
            return;
        };
        pocopine::watch_scope_field_scoped::<String, _>(item_scope, "value", move |v, _| {
            let v = v.clone();
            handle.update(|s| s.text = v);
        });
    }
}

// ── ItemDelete ────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineTagsInputItemDelete.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineTagsInputItemDelete {
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineTagsInputItemDelete {
    pub fn click(&mut self) {
        if self.disabled {
            return;
        }
        // Resolve "my" tag value lazily from the enclosing
        // Item's scope at click time — avoids any cached-at-setup
        // staleness if the chip was reordered or the value
        // binding fired late.
        let Some(item_scope) = ITEM.inject() else {
            return;
        };
        let Some(scope) = Scope::find(item_scope) else {
            return;
        };
        let v = scope
            .state
            .borrow()
            .get("value")
            .as_string()
            .unwrap_or_default();
        if v.is_empty() {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineTagsInputRoot| r.remove_value(v));
        }
    }
}

// ── Input ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineTagsInputInput.poco",
    role = "panel",
    display = "contents"
)]
pub struct PineTagsInputInput {
    #[prop]
    pub placeholder: String,
    #[observe(ROOT)]
    pub disabled: bool,
}

#[handlers]
impl PineTagsInputInput {
    /// `@keydown.enter.prevent` — read the field value, push it
    /// as a new tag, clear the field.
    pub fn commit(&mut self) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(el) = refs::get_on(scope, "field") else {
            return;
        };
        let Ok(input) = el.dyn_into::<HtmlInputElement>() else {
            return;
        };
        let raw = input.value();
        if raw.trim().is_empty() {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineTagsInputRoot| r.add_tag(raw));
        }
        input.set_value("");
    }

    /// `@keydown.backspace` — only acts when the field is empty.
    /// Otherwise the native backspace edits the input normally.
    pub fn maybe_pop(&mut self) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let Some(el) = refs::get_on(scope, "field") else {
            return;
        };
        let Ok(input) = el.dyn_into::<HtmlInputElement>() else {
            return;
        };
        if !input.value().is_empty() {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineTagsInputRoot| r.remove_last());
        }
    }
}

// ── Clear ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "PineTagsInputClear.poco",
    role = "interactive",
    display = "contents"
)]
pub struct PineTagsInputClear {
    #[observe(ROOT)]
    pub disabled: bool,
    /// Mirrored so templates can `pp-show="values.length > 0"` to
    /// hide the Clear button when there's nothing to clear.
    #[observe(ROOT)]
    pub values: Vec<String>,
}

#[handlers]
impl PineTagsInputClear {
    pub fn click(&mut self) {
        if self.disabled {
            return;
        }
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineTagsInputRoot| r.clear());
        }
    }
}
