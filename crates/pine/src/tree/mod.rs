//! `<pine-tree-*>` — ARIA tree (compound).
//!
//! Two parts. Radix-Vue's `TreeVirtualizer` is not included in v0;
//! virtualization is a separate RFC that'll layer over any long
//! list primitive.
//!
//! - **Root** (`pine-tree-root`) — owns the selection model
//!   (`type="single"` / `"multiple"`), the selected `value` /
//!   `values`, and the `expanded` value set. Installs
//!   `pp-roving.virtual` over `[role=treeitem]:not([aria-hidden])`
//!   so arrow keys walk the visible items while DOM focus stays
//!   on Root. Provides its Handle to descendants.
//! - **Item** (`pine-tree-item`) — `role="treeitem"`. Mirrors
//!   `selected` + `expanded` from Root. Computes its `aria-level`
//!   and `aria-hidden` from its ancestor chain so collapsed
//!   subtrees are excluded from both screen-reader output and
//!   keyboard navigation.
//!
//! ```html
//! <pine-tree-root type="single" pp-model:value="active_path">
//!   <pine-tree-item value="src">
//!     <button type="button" @click.stop="toggle">▸</button>
//!     src/
//!     <pine-tree-item value="src/lib.rs">lib.rs</pine-tree-item>
//!     <pine-tree-item value="src/main.rs">main.rs</pine-tree-item>
//!   </pine-tree-item>
//! </pine-tree-root>
//! ```
//!
//! Keyboard (Root hosts focus, items receive virtual highlight):
//!
//! | Key            | Action                                        |
//! |----------------|-----------------------------------------------|
//! | ↑ / ↓          | previous / next visible item                  |
//! | Home / End     | first / last visible item                     |
//! | Enter / Space  | select the highlighted item                   |
//! | →              | expand the highlighted item                   |
//! | ←              | collapse the highlighted item                 |

use pocopine::prelude::*;
use pocopine::{create_context, current_scope_id, refs};
use serde::{Deserialize, Serialize};

create_context!(ROOT: Handle<PineTreeRoot>);
create_context!(ITEM: ScopeId);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineTreeRoot.poco", role = "scope")]
// RFC 049 — Tree's Root holds Items at the top level. Items
// themselves nest (Item→Item) but that's the Item's slot
// contract; here we reject bare leaves and unrelated content.
#[slot(default, only = [PineTreeItem])]
pub struct PineTreeRoot {
    /// `"single"` (default) or `"multiple"`. Drives
    /// `aria-multiselectable` on Root.
    #[prop]
    pub r#type: String,
    /// Currently selected value for `type="single"`. `""` for
    /// "no selection".
    #[model]
    pub value: String,
    /// Currently selected values for `type="multiple"`.
    #[model]
    pub values: Vec<String>,
    /// Expanded item values. Serialized as Vec for the prop /
    /// serde round-trip; membership checks are linear scans
    /// (typical trees have ≤ a few hundred expanded nodes).
    #[prop]
    pub expanded: Vec<String>,
    /// DOM id of the Root element — used as the listbox container
    /// for `pp-roving.virtual`. Derived from the Root's scope id.
    pub tree_id: String,
}

impl Default for PineTreeRoot {
    fn default() -> Self {
        Self {
            r#type: "single".into(),
            value: String::new(),
            values: Vec::new(),
            expanded: Vec::new(),
            tree_id: String::new(),
        }
    }
}

#[handlers]
impl PineTreeRoot {
    fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            self.tree_id = format!("pine-tree-{}", scope.0);
        }
        ROOT.provide(this::<Self>());
    }

    fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root_el) = refs.get("tree") else {
            return;
        };
        let tree_id = self.tree_id.clone();
        // Defer the install: Items must be in the DOM first so
        // the initial activedescendant seeds against a real item.
        pocopine::tick::after_flush(move || {
            pocopine_core::directives::roving::install_virtual_on(
                &root_el,
                &tree_id,
                &[],
                Some("[role=\"treeitem\"]"),
            );
        });
    }

    /// Select the item currently virtually-highlighted (Enter / Space).
    pub fn select_active(&mut self) {
        let Some(val) = active_item_value() else {
            return;
        };
        self.select_value(val);
    }

    /// Expand the highlighted item (→).
    pub fn expand_active(&mut self) {
        let Some(val) = active_item_value() else {
            return;
        };
        self.expand_item(val);
    }

    /// Collapse the highlighted item (←).
    pub fn collapse_active(&mut self) {
        let Some(val) = active_item_value() else {
            return;
        };
        self.collapse_item(val);
    }

    /// Called by Item's click handler after the user clicks it.
    pub fn select_value(&mut self, v: String) {
        match self.r#type.as_str() {
            "multiple" => {
                if let Some(i) = self.values.iter().position(|x| x == &v) {
                    self.values.remove(i);
                } else {
                    self.values.push(v);
                }
            }
            _ => {
                if self.value != v {
                    self.value = v;
                }
            }
        }
    }

    pub fn toggle_item(&mut self, v: String) {
        if let Some(i) = self.expanded.iter().position(|x| x == &v) {
            self.expanded.remove(i);
        } else {
            self.expanded.push(v);
        }
    }

    pub fn expand_item(&mut self, v: String) {
        if !self.expanded.iter().any(|x| x == &v) {
            self.expanded.push(v);
        }
    }

    pub fn collapse_item(&mut self, v: String) {
        if let Some(i) = self.expanded.iter().position(|x| x == &v) {
            self.expanded.remove(i);
        }
    }
}

impl PineTreeRoot {
    pub fn is_selected(&self, v: &str) -> bool {
        match self.r#type.as_str() {
            "multiple" => self.values.iter().any(|x| x == v),
            _ => self.value == v,
        }
    }
    pub fn is_expanded(&self, v: &str) -> bool {
        self.expanded.iter().any(|x| x == v)
    }
}

fn active_item_value() -> Option<String> {
    let scope = current_scope_id()?;
    let tree_el = refs::get_on(scope, "tree")?;
    let active_id = tree_el.get_attribute("aria-activedescendant")?;
    let doc = web_sys::window()?.document()?;
    let item_el = doc.get_element_by_id(&active_id)?;
    item_el.get_attribute("value")
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTreeItem.poco", role = "panel")]
// RFC 049 — a TreeItem typically contains its label + an
// optional ItemToggle + nested Items (for branches). `accepts`
// because the label content is author-owned (text, icons,
// custom markup); the pocopine parts we recognize are the
// toggle and nested items.
#[slot(default, accepts = [PineTreeItemToggle, PineTreeItem])]
pub struct PineTreeItem {
    #[prop]
    pub value: String,
    #[prop]
    pub disabled: bool,
    /// 1-based depth for `aria-level`.
    pub level: u32,
    /// True when this item has at least one descendant
    /// `pine-tree-item`. Drives whether `aria-expanded` is set
    /// (leaves shouldn't have `aria-expanded` per WAI-ARIA).
    pub has_children: bool,
    /// Reactive mirror of `root.is_expanded(self.value)`.
    pub expanded: bool,
    /// Reactive mirror of `root.is_selected(self.value)`.
    pub selected: bool,
    /// True when **every** ancestor `pine-tree-item` is currently
    /// expanded. When false, Item renders `aria-hidden="true"` so
    /// it's skipped by the virtual roving's `is_nav_candidate`
    /// check and invisible to screen readers.
    pub parent_visible: bool,
    /// Stable DOM id for `aria-activedescendant`.
    pub item_id: String,
    /// Ancestor value chain, in order closest-first. Seeded at
    /// `on_ready` time from the DOM; used to recompute
    /// `parent_visible` each time Root's `expanded` changes.
    pub ancestor_values: Vec<String>,
}

#[handlers]
impl PineTreeItem {
    fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            self.item_id = format!("pine-tree-item-{}", scope.0);
            // Make this Item's scope id injectable by any Toggle
            // sub-component the author places in our slot — slot
            // content binds to the CALLER's scope for directive
            // dispatch, so the Toggle can't `@click="toggle"`
            // us directly.
            ITEM.provide(scope);
        }
        self.parent_visible = true;
        if let Some(root) = ROOT.inject() {
            let my_value = self.value.clone();
            let (sel, exp) = root.with(|r| (r.is_selected(&my_value), r.is_expanded(&my_value)));
            self.selected = sel;
            self.expanded = exp;
        }
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        let Some(root) = ROOT.inject() else {
            return;
        };
        let Some(el) = refs.get("item") else { return };

        let ancestor_values = collect_ancestor_values(&el);
        let level = (ancestor_values.len() as u32) + 1;
        let my_value = handle.with(|s| s.value.clone());
        let initial_parent_visible =
            compute_parent_visible(&ancestor_values, &root.with(|r| r.expanded.clone()));

        // Initial writes deferred via `tick::next` — `on_ready`
        // executes under an immutable borrow on `scope.state`
        // (walker.rs:193), so a synchronous `handle.update` would
        // double-borrow. Deferring lets the on_ready frame unwind
        // first.
        let handle_for_init = handle.clone();
        let ancestor_values_for_init = ancestor_values.clone();
        pocopine::tick::next(move || {
            handle_for_init.update(|s| {
                s.level = level;
                s.ancestor_values = ancestor_values_for_init;
                s.parent_visible = initial_parent_visible;
            });
        });

        // `has_children` has to wait for descendants to mount.
        let handle_for_children = handle.clone();
        let el_for_children = el.clone();
        pocopine::tick::after_flush(move || {
            let has = el_for_children
                .query_selector("pine-tree-item")
                .ok()
                .flatten()
                .is_some();
            handle_for_children.update(|s| s.has_children = has);
        });

        // Selection mirror. Watches both `value` (single) and
        // `values` (multiple) — one of the two applies.
        let root_scope = root.scope_id();
        let my_value_sel = my_value.clone();
        let h_sel_1 = handle.clone();
        let root_for_sel_1 = root.clone();
        pocopine::watch_scope_field_scoped::<String, _>(root_scope, "value", move |_, _| {
            let is_sel = root_for_sel_1.with(|r| r.is_selected(&my_value_sel));
            h_sel_1.update(|s| s.selected = is_sel);
        });
        let my_value_sel2 = my_value.clone();
        let h_sel_2 = handle.clone();
        let root_for_sel_2 = root.clone();
        pocopine::watch_scope_field_scoped::<Vec<String>, _>(root_scope, "values", move |_, _| {
            let is_sel = root_for_sel_2.with(|r| r.is_selected(&my_value_sel2));
            h_sel_2.update(|s| s.selected = is_sel);
        });

        // Expansion mirror + parent_visible recompute.
        let my_value_exp = my_value;
        let ancestor_values_exp = ancestor_values;
        let h_exp = handle;
        let root_for_exp = root;
        pocopine::watch_scope_field_scoped::<Vec<String>, _>(
            root_scope,
            "expanded",
            move |v, _| {
                let is_exp = v.iter().any(|x| x == &my_value_exp);
                let vis = compute_parent_visible(&ancestor_values_exp, v);
                h_exp.update(|s| {
                    s.expanded = is_exp;
                    s.parent_visible = vis;
                });
                let _ = &root_for_exp;
            },
        );
    }

    /// `@click.stop` on the Item root. Selection only — expansion
    /// is a separate `@click.stop="toggle"` on the author's
    /// chevron button.
    pub fn select(&mut self) {
        if self.disabled {
            return;
        }
        let v = self.value.clone();
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineTreeRoot| r.select_value(v));
        }
    }

    pub fn toggle(&mut self) {
        if self.disabled {
            return;
        }
        let v = self.value.clone();
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineTreeRoot| r.toggle_item(v));
        }
    }
}

fn collect_ancestor_values(item_el: &web_sys::Element) -> Vec<String> {
    // `item_el` is this Item's rendered inner `<div>` (where
    // `pp-ref="item"` sits). Its immediate parent is this
    // component's own `<pine-tree-item>` custom tag — that's
    // SELF, not an ancestor. Skip it before walking.
    let mut out = Vec::new();
    let mut cur = item_el
        .parent_element()
        .and_then(|own_tag| own_tag.parent_element());
    while let Some(el) = cur {
        if el.tag_name().eq_ignore_ascii_case("pine-tree-item") {
            if let Some(v) = el.get_attribute("value") {
                out.push(v);
            }
        }
        cur = el.parent_element();
    }
    out
}

fn compute_parent_visible(ancestor_values: &[String], expanded: &[String]) -> bool {
    ancestor_values
        .iter()
        .all(|a| expanded.iter().any(|e| e == a))
}

// ── Toggle ────────────────────────────────────────────────────────

/// `<pine-tree-item-toggle>` — click target that expands /
/// collapses its enclosing Item. Mirrors the Item's `expanded`
/// + `has_children` so templates can show / hide it and rotate
/// the chevron.
///
/// Needed because the Item's slot content is dispatched in the
/// CALLER's scope (see `walker::materialize_slot`) — a plain
/// `<button @click="toggle">` inside the slot would look up
/// `toggle` on the caller, not on the Item. The Toggle
/// component sidesteps that by being its own scope and
/// reaching the Item via `inject`.
#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineTreeItemToggle.poco", role = "interactive")]
pub struct PineTreeItemToggle {
    pub expanded: bool,
    pub has_children: bool,
}

#[handlers]
impl PineTreeItemToggle {
    fn on_setup(&mut self) {
        if let Some(item) = ITEM.inject() {
            if let Some(scope) = Scope::find(item) {
                let s = scope.state.borrow();
                self.expanded = s.get("expanded").as_bool().unwrap_or(false);
                self.has_children = s.get("has_children").as_bool().unwrap_or(false);
            }
        }
    }

    fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(item) = ITEM.inject() else {
            return;
        };
        let h_exp = handle.clone();
        pocopine::watch_scope_field_scoped::<bool, _>(item, "expanded", move |&v, _| {
            h_exp.update(|s| s.expanded = v);
        });
        let h_has = handle;
        pocopine::watch_scope_field_scoped::<bool, _>(item, "has_children", move |&v, _| {
            h_has.update(|s| s.has_children = v);
        });
    }

    pub fn click(&mut self) {
        let Some(item_scope) = ITEM.inject() else {
            return;
        };
        let Some(scope) = Scope::find(item_scope) else {
            return;
        };
        let Some(inner) = scope.typed::<PineTreeItem>() else {
            return;
        };
        Handle::new(inner, item_scope).update(|s: &mut PineTreeItem| s.toggle());
    }
}
