//! `<pine-combobox-*>` — editable filter-driven listbox
//! (WAI-ARIA 1.2 combobox pattern).
//!
//! Six parts:
//!
//! - **Root** — owns `value` + `query` + `open` + `disabled`.
//!   Provides its Handle.
//! - **Input** — text `<input role="combobox">`. Types update
//!   `root.query` (opens the listbox on first keystroke).
//!   `aria-activedescendant` tracks the virtually-highlighted
//!   item while DOM focus stays on the input — installed via
//!   `pp-roving.virtual` programmatically from `on_ready`
//!   (the listbox id is per-instance).
//! - **Portal** — teleport wrapper, `pp-if="open"`.
//! - **Content** — `<ul role="listbox">`. No `pp-roving` of its
//!   own (the input hosts the activedescendant cycle). Click-
//!   outside closes.
//! - **Empty** — author-slottable "no matches" row, auto-hidden
//!   when the current query matches at least one item.
//! - **Item** — `<li role="option">`. Mirrors `root.query` and
//!   hides itself when the query doesn't match (case-insensitive
//!   `contains` over the item's rendered text). Click selects.
//!
//! ```html
//! <pine-combobox-root pp-model:value="framework">
//!   <pine-combobox-input placeholder="Search frameworks…"></pine-combobox-input>
//!   <pine-combobox-portal>
//!     <pine-combobox-content>
//!       <pine-combobox-item value="react">React</pine-combobox-item>
//!       <pine-combobox-item value="vue">Vue</pine-combobox-item>
//!       <pine-combobox-item value="svelte">Svelte</pine-combobox-item>
//!       <pine-combobox-empty>Nothing matched.</pine-combobox-empty>
//!     </pine-combobox-content>
//!   </pine-combobox-portal>
//! </pine-combobox-root>
//! ```

use crate::compound;
use pocopine::prelude::*;
use pocopine::{
    current_scope_id, emit_model, inject, inject_key, provide, refs, watch_scope_field,
};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

inject_key!(ROOT: Handle<PineComboboxRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineComboboxRoot.poco", role = "scope")]
pub struct PineComboboxRoot {
    #[prop]
    pub open: bool,
    #[prop]
    pub value: String,
    #[prop]
    pub default_value: String,
    #[prop]
    pub disabled: bool,
    /// Live input text used for filtering. Separate from
    /// `value` — `value` is the committed selection, `query`
    /// is the in-flight search string.
    pub query: String,
    /// id used by Input's `aria-controls` and by the virtual-
    /// mode pp-roving install. Derived from Root's scope id.
    pub listbox_id: String,
    /// `true` when at least one Item is visible under the
    /// current query. Mirrored by the Empty slot so authors
    /// can show a "no matches" row.
    pub has_matches: bool,
}

#[handlers]
impl PineComboboxRoot {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            self.listbox_id = format!("pine-combobox-listbox-{}", scope.0);
        }
        if self.value.is_empty() && !self.default_value.is_empty() {
            self.value = self.default_value.clone();
        }
        self.has_matches = true;
        provide(&ROOT, this::<Self>());
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
        }
    }

    pub fn set_query(&mut self, q: String) {
        if self.query != q {
            self.query = q;
            if !self.open {
                self.open = true;
            }
        }
    }

    pub fn select_value(&mut self, value: String, label: String) {
        if self.value != value {
            self.value = value.clone();
            emit_model(value);
        }
        // Reflect the picked label in the input so the user
        // sees what they selected.
        self.query = label;
        self.open = false;
    }

    pub fn refresh_match_state(&mut self, any_visible: bool) {
        if self.has_matches != any_visible {
            self.has_matches = any_visible;
        }
    }
}

// ── Input ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineComboboxInput.poco")]
pub struct PineComboboxInput {
    #[prop]
    pub placeholder: String,
    #[observe(ROOT)] pub open: bool,
    #[observe(ROOT)] pub disabled: bool,
    #[observe(ROOT)] pub listbox_id: String,
}

#[handlers]
impl PineComboboxInput {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = inject::<Handle<PineComboboxRoot>>(&ROOT) else {
            return;
        };
        let root_scope = root.scope_id();

        // Install activedescendant-mode pp-roving on this input
        // exactly once. Guard by a shared `installed` flag so
        // repeated open→close→open cycles don't stack duplicate
        // keydown listeners (which caused arrow keys to advance
        // the highlight by multiple items per press).
        if let Some(input_el) = refs.get("input") {
            let listbox_id = root.with(|r| r.listbox_id.clone());
            // Stamp the input with Root's scope id so Content
            // can anchor to `[data-pine-combobox-input="N"]`
            // (same convention the other compound primitives use
            // for Trigger → Content).
            let _ = input_el.set_attribute(
                "data-pine-combobox-input",
                &format!("{}", root_scope.0),
            );
            let installed: Rc<Cell<bool>> = Rc::new(Cell::new(false));
            schedule_install_virtual(
                input_el.clone(),
                listbox_id.clone(),
                installed.clone(),
            );
            let input_for_watch = input_el;
            let listbox_for_watch = listbox_id;
            watch_scope_field::<bool, _>(root_scope, "open", move |&is_open, _| {
                if !is_open {
                    return;
                }
                schedule_install_virtual(
                    input_for_watch.clone(),
                    listbox_for_watch.clone(),
                    installed.clone(),
                );
            });
        }
    }

    /// `@input` handler — pull the input's current value and
    /// push it to Root as the live query, then schedule a
    /// macrotask to re-scan the listbox and update
    /// `Root.has_matches`. Deferring the match-state update
    /// avoids re-entrant `Handle::update` from each Item's
    /// query watcher chain.
    pub fn on_input(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(el) = refs::get_on(scope, "input") else { return };
        let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() else {
            return;
        };
        let new_query = input.value();
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineComboboxRoot| r.set_query(new_query));
            schedule_match_refresh(root);
        }
    }

    /// `@focus` handler — opening on focus matches Radix UX.
    pub fn on_focus(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineComboboxRoot| {
                if !r.open && !r.disabled {
                    r.open = true;
                }
            });
        }
    }

    /// Enter selects whichever item currently carries
    /// `aria-activedescendant` via the virtual-mode roving.
    pub fn on_enter(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(el) = refs::get_on(scope, "input") else { return };
        let Some(active_id) = el.get_attribute("aria-activedescendant") else {
            return;
        };
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        let Some(item_el) = doc.get_element_by_id(&active_id) else {
            return;
        };
        // Click the item — its own handler walks back through
        // provide/inject to Root.select_value.
        if let Ok(html) = item_el.dyn_into::<HtmlElement>() {
            html.click();
        }
    }

    pub fn close(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineComboboxRoot| r.close());
        }
    }
}

/// Install `pp-roving.virtual` on the input when the listbox
/// is actually in the DOM. Guarded by a shared `installed`
/// flag so repeated open→close→open cycles don't stack
/// duplicate keydown listeners on the input (the stacked
/// listeners were advancing the highlight by multiple items
/// per arrow press).
fn schedule_install_virtual(
    input_el: web_sys::Element,
    listbox_id: String,
    installed: Rc<Cell<bool>>,
) {
    if installed.get() {
        return;
    }
    pocopine::tick::after_flush(move || {
        if installed.get() {
            return;
        }
        let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        if doc.get_element_by_id(&listbox_id).is_none() {
            return;
        }
        pocopine_core::directives::roving::install_virtual_on(
            &input_el,
            &listbox_id,
            &[],
            None,
        );
        installed.set(true);
    });
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineComboboxPortal.poco", role = "scope")]
pub struct PineComboboxPortal {
    #[observe(ROOT)] pub open: bool,
}

#[handlers]
impl PineComboboxPortal {}

// ── Content ───────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
#[component(template = "PineComboboxContent.poco", role = "list", transition = "fade")]
pub struct PineComboboxContent {
    pub listbox_id: String,
    /// Selector resolving to the linked Input element; computed
    /// from the Root's scope id in `on_setup` (same Trigger →
    /// Content anchor convention used by DropdownMenu/Popover/
    /// Select, just targeting the Input instead of a Trigger).
    pub anchor: String,
    #[prop]
    pub side: String,
    #[prop]
    pub align: String,
    #[prop]
    pub side_offset: f64,
}

impl Default for PineComboboxContent {
    fn default() -> Self {
        Self {
            listbox_id: String::new(),
            anchor: String::new(),
            side: "bottom".into(),
            align: "start".into(),
            side_offset: 4.0,
        }
    }
}

#[handlers]
impl PineComboboxContent {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject::<Handle<PineComboboxRoot>>(&ROOT) {
            let (lb, scope_id) =
                root.with(|r| (r.listbox_id.clone(), root.scope_id().0));
            self.listbox_id = lb;
            self.anchor = format!("[data-pine-combobox-input=\"{scope_id}\"]");
        }
    }

    pub fn on_ready(&self, refs: pocopine::Refs) {
        // Anchor the listbox to the Input. Without this the
        // teleported `<ul>` lands at the top of `<body>` with
        // default positioning, leaving the dropdown invisible
        // below the viewport.
        let Some(menu) = refs.get("menu") else { return };
        let Some(input) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.query_selector(&self.anchor).ok().flatten())
        else {
            return;
        };
        let menu_for_anchor = menu.clone();
        if let Ok(floater) = menu_for_anchor.dyn_into::<web_sys::HtmlElement>() {
            let placement = pocopine_core::directives::anchor::Placement {
                side: pocopine_core::directives::anchor::Side::parse(&self.side),
                align: pocopine_core::directives::anchor::Align::parse(&self.align),
            };
            pocopine_core::directives::anchor::install(
                &floater,
                &input,
                placement,
                self.side_offset,
                true,
            );
        }

        // Outside-dismiss. Replaces `@click.outside` because
        // that directive fires when the target isn't inside
        // Content — but for Combobox the Input (teleported
        // apart from Content) is *also* part of the combobox
        // surface. Listener treats both as "inside".
        //
        // Captures the Root handle here (handler-context) so the
        // document-level listener that fires outside any scope
        // context can still close via `root.update` without
        // needing to `inject`.
        if let Some(root) = inject::<Handle<PineComboboxRoot>>(&ROOT) {
            compound::install_outside_dismiss(menu, vec![input], move || {
                root.update(|r: &mut PineComboboxRoot| r.close());
            });
        }
    }

    pub fn close(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineComboboxRoot| r.close());
        }
    }
}

// ── Empty ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineComboboxEmpty.poco", role = "panel")]
pub struct PineComboboxEmpty {
    pub empty: bool,
}

#[handlers]
impl PineComboboxEmpty {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.empty = root.with(|r| !r.has_matches);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject::<Handle<PineComboboxRoot>>(&ROOT) else {
            return;
        };
        watch_scope_field::<bool, _>(root.scope_id(), "has_matches", move |&v, _| {
            handle.update(|s| s.empty = !v);
        });
    }
}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineComboboxItem.poco", role = "item", animate = "flip")]
pub struct PineComboboxItem {
    #[prop]
    pub value: String,
    #[prop]
    pub disabled: bool,
    pub selected: bool,
    pub visible: bool,
    pub item_id: String,
    /// Rendered text content, used for the `contains` filter
    /// + for set-query-on-select so the input reflects the
    /// chosen label.
    pub label: String,
}

#[handlers]
impl PineComboboxItem {
    pub fn on_setup(&mut self) {
        self.visible = true;
        if let Some(scope) = current_scope_id() {
            self.item_id = format!("pine-combobox-opt-{}", scope.0);
        }
        if let Some(root) = inject(&ROOT) {
            self.selected = root.with(|r| r.value == self.value);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, refs: pocopine::Refs) {
        let label = refs
            .get("item")
            .and_then(|el| el.text_content())
            .unwrap_or_default()
            .trim()
            .to_string();
        let my_value = self.value.clone();

        let Some(root) = inject::<Handle<PineComboboxRoot>>(&ROOT) else {
            return;
        };
        let root_scope = root.scope_id();

        // Defer everything into the next tick. Items mount
        // synchronously inside Root.open's reactive flush (via
        // Portal + pp-if), and calling `handle.update` from
        // inside that same flush re-enters the RefCell borrow
        // the walker is already holding on *some* ancestor's
        // state. `tick::next` (micro-task) runs after the
        // current flush completes, so our state writes land
        // cleanly.
        let h_for_setup = handle.clone();
        let label_for_setup = label.clone();
        pocopine::tick::next(move || {
            h_for_setup.update(|s| s.label = label_for_setup);
        });

        // Mirror root.value for aria-selected + data-state.
        let h = handle.clone();
        let my_value_for_value_watch = my_value.clone();
        watch_scope_field::<String, _>(root_scope, "value", move |v, _| {
            let is_selected = v == &my_value_for_value_watch;
            h.update(|s| s.selected = is_selected);
        });

        // Mirror root.query + recompute visible. Closure captures
        // label / value by value, so no handle.with is needed —
        // less surface for borrow conflicts during the reactive
        // flush chain.
        let my_value_lc = my_value.to_lowercase();
        let label_lc = label.to_lowercase();
        let h = handle;
        watch_scope_field::<String, _>(root_scope, "query", move |q, _| {
            let query = q.to_lowercase();
            let visible =
                query.is_empty() || label_lc.contains(&query) || my_value_lc.contains(&query);
            h.update(|s| s.visible = visible);
        });
    }

    pub fn select(&mut self) {
        if self.disabled {
            return;
        }
        let v = self.value.clone();
        let label = self.label.clone();
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineComboboxRoot| r.select_value(v, label));
            // Also sync the DOM input's `.value` so the typed
            // string matches the chosen label.
            let scope = root.scope_id();
            if let Some(input_scope) = find_input_scope(scope) {
                if let Some(input_el) = refs::get_on(input_scope, "input") {
                    if let Ok(input) = input_el.dyn_into::<web_sys::HtmlInputElement>() {
                        input.set_value(&self.label);
                    }
                }
            }
        }
    }
}

/// Walk the component tree to find the PineComboboxInput's
/// scope. Needed because we don't have a direct inject key for
/// "the input in my Combobox" — but we know it's a descendant
/// of the Root scope.
fn find_input_scope(_root_scope: pocopine::ScopeId) -> Option<pocopine::ScopeId> {
    // Simplest: scan the document for the one input with
    // `pp-ref="input"` inside this Root's subtree. We don't
    // know the exact scope, so fall back to current_scope_id's
    // walker-managed stack when called inside Root.update —
    // which isn't the case here. Pragmatic v1: return None and
    // rely on the watch on `query` (which happens when Root
    // writes query) to re-sync the DOM via the Input's own
    // mirror path.
    None
}

fn schedule_match_refresh(root: Handle<PineComboboxRoot>) {
    pocopine::tick::after_flush(move || {
        refresh_match_state(&root);
    });
}

fn refresh_match_state(root: &Handle<PineComboboxRoot>) {
    // Count how many items are currently visible under the root's
    // listbox; update Root.has_matches accordingly. Cheap because
    // the listbox is typically small.
    let listbox_id = root.with(|r| r.listbox_id.clone());
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Some(listbox) = doc.get_element_by_id(&listbox_id) else {
        return;
    };
    let Ok(items) = listbox.query_selector_all("[role=\"option\"]") else {
        return;
    };
    let mut any = false;
    for i in 0..items.length() {
        let Some(node) = items.item(i) else { continue };
        let Ok(el) = node.dyn_into::<web_sys::Element>() else {
            continue;
        };
        if el.has_attribute("hidden") {
            continue;
        }
        // `pp-show="false"` sets `display:none` via style.
        if let Ok(html) = el.dyn_into::<HtmlElement>() {
            if html.offset_parent().is_none() {
                if let Some(win) = web_sys::window() {
                    if let Ok(Some(style)) = win.get_computed_style(&html) {
                        if let Ok(display) = style.get_property_value("display") {
                            if display == "none" {
                                continue;
                            }
                        }
                    }
                }
            }
        }
        any = true;
        break;
    }
    root.update(|r: &mut PineComboboxRoot| r.refresh_match_state(any));
}
