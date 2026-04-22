//! `<pine-command-*>` — modal command palette (Cmd+K style).
//!
//! Composes Dialog's modal shell with a Combobox-style filter so
//! authors get a full command-palette UX without hand-rolling
//! the pieces. Eight parts:
//!
//! - **Root** — owns `open`, `query`, and an optional global
//!   `shortcut` prop (default `"Mod+k"`) that installs a
//!   document-level keydown listener to toggle the palette.
//!   Provides its Handle to descendants.
//! - **Portal** — teleports the modal to `<body>` when `open`.
//! - **Overlay** — fullscreen backdrop; click-self closes.
//! - **Content** — centered dialog panel (`role="dialog"` +
//!   `aria-modal="true"`). Escape closes.
//! - **Input** — search `<input role="combobox">`. Types
//!   update `root.query`; Enter activates the currently
//!   virtually-highlighted Item via `aria-activedescendant`.
//! - **List** — `<ul role="listbox">` container for Items.
//!   The Input installs `pp-roving.virtual` scoped to this
//!   list's id.
//! - **Item** — `<li role="option">`. Authors wire
//!   `@select="my_handler"` on the tag; Item fires
//!   `pp:command:select` (cancelable) then closes the palette.
//!   Click or Enter both route through `select`.
//! - **Empty** — `pp-show`-gated "no commands match" row; binds
//!   to `!root.has_matches`.
//!
//! ```html
//! <pine-command-root shortcut="Mod+k">
//!   <pine-command-portal>
//!     <pine-command-overlay></pine-command-overlay>
//!     <pine-command-content>
//!       <pine-command-input placeholder="Type a command…"></pine-command-input>
//!       <pine-command-list>
//!         <pine-command-item value="new"  @pp:command:select="create_file">New File</pine-command-item>
//!         <pine-command-item value="open" @pp:command:select="open_file">Open…</pine-command-item>
//!         <pine-command-empty>No commands match.</pine-command-empty>
//!       </pine-command-list>
//!     </pine-command-content>
//!   </pine-command-portal>
//! </pine-command-root>
//! ```

use pocopine::prelude::*;
use pocopine::{current_scope_id, emit_model, inject, inject_key, provide, refs, watch_scope_field};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::rc::Rc;
use std::cell::RefCell;
use std::collections::HashMap;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{EventTarget, HtmlElement, KeyboardEvent};

inject_key!(ROOT: Handle<PineCommandRoot>);

// Per-Root runtime — holds the global shortcut listener so
// `on_unmount` can detach it cleanly.
struct RootRuntime {
    shortcut: Option<Closure<dyn FnMut(KeyboardEvent)>>,
}

thread_local! {
    static ROOT_RUNTIME: RefCell<HashMap<ScopeId, RootRuntime>> =
        RefCell::new(HashMap::new());
}

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandRoot.poco", role = "scope", display = "contents")]
pub struct PineCommandRoot {
    #[prop]
    pub open: bool,
    /// Live search query. Mirrored into Items' `visible` via
    /// `watch_scope_field`, same pattern as PineCombobox.
    pub query: String,
    /// Global shortcut key combo that toggles the palette.
    /// Format: `Mod+<key>` or `Ctrl+<key>` / `Meta+<key>`. The
    /// `Mod` alias maps to Meta on macOS, Ctrl elsewhere. Default
    /// `"Mod+k"`.
    #[prop]
    pub shortcut: String,
    /// Skip the global shortcut install entirely. Useful when
    /// Command is wrapped inside a Popover / Dropdown whose own
    /// trigger opens it — the outer compound already owns
    /// open/close, and a lingering Cmd+K would toggle an
    /// unrelated `Root.open` out from under it.
    #[prop]
    pub no_shortcut: bool,
    pub listbox_id: String,
    pub has_matches: bool,
}

#[handlers]
impl PineCommandRoot {
    pub fn on_setup(&mut self) {
        if let Some(scope) = current_scope_id() {
            self.listbox_id = format!("pine-command-list-{}", scope.0);
        }
        if self.shortcut.is_empty() {
            self.shortcut = "Mod+k".into();
        }
        self.has_matches = true;
        provide(&ROOT, this::<Self>());
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>, scope: ScopeId) {
        if self.no_shortcut {
            return;
        }
        install_global_shortcut(scope, handle, self.shortcut.clone());
    }

    pub fn on_unmount(&mut self) {
        if let Some(scope) = current_scope_id() {
            teardown_global_shortcut(scope);
        }
    }

    pub fn open_self(&mut self) {
        if !self.open {
            self.open = true;
            self.query = String::new();
            self.has_matches = true;
            emit_model(true);
        }
    }

    pub fn close(&mut self) {
        if self.open {
            self.open = false;
            self.query = String::new();
            emit_model(false);
        }
    }

    pub fn toggle(&mut self) {
        if self.open {
            self.close();
        } else {
            self.open_self();
        }
    }

    pub fn set_query(&mut self, q: String) {
        if self.query != q {
            self.query = q;
        }
    }

    pub fn refresh_match_state(&mut self, any_visible: bool) {
        if self.has_matches != any_visible {
            self.has_matches = any_visible;
        }
    }
}

fn install_global_shortcut(
    scope: ScopeId,
    handle: Handle<PineCommandRoot>,
    shortcut: String,
) {
    let parsed = parse_shortcut(&shortcut);
    let cb = Closure::wrap(Box::new({
        move |ev: KeyboardEvent| {
            if matches_shortcut(&ev, &parsed) {
                ev.prevent_default();
                handle.update(|r: &mut PineCommandRoot| r.toggle());
            }
        }
    }) as Box<dyn FnMut(KeyboardEvent)>);
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let target: &EventTarget = doc.as_ref();
        let _ =
            target.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
    }
    ROOT_RUNTIME.with(|r| {
        r.borrow_mut().insert(
            scope,
            RootRuntime {
                shortcut: Some(cb),
            },
        );
    });
}

fn teardown_global_shortcut(scope: ScopeId) {
    let Some(rt) = ROOT_RUNTIME.with(|r| r.borrow_mut().remove(&scope)) else {
        return;
    };
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        let target: &EventTarget = doc.as_ref();
        if let Some(cb) = rt.shortcut.as_ref() {
            let _ = target
                .remove_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
        }
    }
}

#[derive(Clone)]
struct ShortcutMatch {
    key: String,
    needs_mod: bool,
    needs_ctrl: bool,
    needs_meta: bool,
    needs_shift: bool,
    needs_alt: bool,
}

fn parse_shortcut(spec: &str) -> ShortcutMatch {
    let mut out = ShortcutMatch {
        key: String::new(),
        needs_mod: false,
        needs_ctrl: false,
        needs_meta: false,
        needs_shift: false,
        needs_alt: false,
    };
    for part in spec.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "mod" => out.needs_mod = true,
            "ctrl" | "control" => out.needs_ctrl = true,
            "meta" | "cmd" | "command" => out.needs_meta = true,
            "shift" => out.needs_shift = true,
            "alt" | "option" => out.needs_alt = true,
            "" => {}
            k => out.key = k.to_string(),
        }
    }
    out
}

fn matches_shortcut(ev: &KeyboardEvent, spec: &ShortcutMatch) -> bool {
    if ev.key().to_ascii_lowercase() != spec.key {
        return false;
    }
    if spec.needs_mod {
        if !(ev.meta_key() || ev.ctrl_key()) {
            return false;
        }
    } else {
        if spec.needs_ctrl && !ev.ctrl_key() {
            return false;
        }
        if spec.needs_meta && !ev.meta_key() {
            return false;
        }
    }
    if spec.needs_shift && !ev.shift_key() {
        return false;
    }
    if spec.needs_alt && !ev.alt_key() {
        return false;
    }
    true
}

// ── Portal ────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandPortal.poco", role = "scope", display = "contents")]
pub struct PineCommandPortal {
    #[observe(ROOT)] pub open: bool,
}

#[handlers]
impl PineCommandPortal {}

// ── Overlay ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandOverlay.poco", role = "panel", transition = "fade")]
pub struct PineCommandOverlay {}

#[handlers]
impl PineCommandOverlay {
    pub fn close(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineCommandRoot| r.close());
        }
    }
}

// ── Content ───────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandContent.poco", role = "panel", transition = "fade-scale")]
pub struct PineCommandContent {}

#[handlers]
impl PineCommandContent {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        // Focus the input once Content mounts.
        let Some(content_el) = refs.get("content") else {
            return;
        };
        if let Some(input_el) = content_el
            .query_selector(".pine-command-input")
            .ok()
            .flatten()
        {
            if let Ok(html) = input_el.dyn_into::<HtmlElement>() {
                let _ = html.focus();
            }
        }
    }

    pub fn close(&mut self) {
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineCommandRoot| r.close());
        }
    }
}

// ── Input ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandInput.poco")]
pub struct PineCommandInput {
    #[prop]
    pub placeholder: String,
    #[observe(ROOT)] pub open: bool,
    #[observe(ROOT)] pub listbox_id: String,
}

#[handlers]
impl PineCommandInput {
    pub fn on_ready(&self, refs: pocopine::Refs) {
        let Some(root) = inject::<Handle<PineCommandRoot>>(&ROOT) else {
            return;
        };
        let root_scope = root.scope_id();

        // Install pp-roving.virtual on the input EXACTLY ONCE.
        // Guarded by a shared flag so repeated open→close→open
        // doesn't stack duplicate keydown listeners on the input
        // (the stack was advancing the highlight by multiple
        // items per arrow press).
        if let Some(input_el) = refs.get("input") {
            let listbox_id = root.with(|r| r.listbox_id.clone());
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

    pub fn on_input(&mut self) {
        let Some(scope) = current_scope_id() else { return };
        let Some(el) = refs::get_on(scope, "input") else { return };
        let Ok(input) = el.dyn_into::<web_sys::HtmlInputElement>() else {
            return;
        };
        let q = input.value();
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineCommandRoot| r.set_query(q));
            schedule_match_refresh(root);
        }
    }

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
        if let Ok(html) = item_el.dyn_into::<HtmlElement>() {
            html.click();
        }
    }
}

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

fn schedule_match_refresh(root: Handle<PineCommandRoot>) {
    pocopine::tick::after_flush(move || {
        refresh_match_state(&root);
    });
}

fn refresh_match_state(root: &Handle<PineCommandRoot>) {
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
    root.update(|r: &mut PineCommandRoot| r.refresh_match_state(any));
}

// ── List ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandList.poco", role = "list", display = "contents")]
pub struct PineCommandList {
    #[observe(ROOT)] pub listbox_id: String,
}

#[handlers]
impl PineCommandList {}

// ── Item ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandItem.poco", role = "item", display = "contents", animate = "flip")]
pub struct PineCommandItem {
    #[prop]
    pub value: String,
    #[prop]
    pub disabled: bool,
    pub visible: bool,
    pub item_id: String,
    pub label: String,
}

#[handlers]
impl PineCommandItem {
    pub fn on_setup(&mut self) {
        self.visible = true;
        if let Some(scope) = current_scope_id() {
            self.item_id = format!("pine-command-opt-{}", scope.0);
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
        let label_lc = label.to_lowercase();
        let value_lc = my_value.to_lowercase();

        // Defer label write so the walker's reactive flush that
        // mounted us has fully released its state borrows.
        let h_label = handle.clone();
        pocopine::tick::next(move || {
            h_label.update(|s| s.label = label);
        });

        let Some(root) = inject::<Handle<PineCommandRoot>>(&ROOT) else {
            return;
        };
        let root_scope = root.scope_id();
        let h = handle;
        watch_scope_field::<String, _>(root_scope, "query", move |q, _| {
            let query = q.to_lowercase();
            let visible =
                query.is_empty() || label_lc.contains(&query) || value_lc.contains(&query);
            h.update(|s| s.visible = visible);
        });
    }

    pub fn select(&mut self) {
        if self.disabled {
            return;
        }
        let prevented = pocopine::emit_cancelable("pp:command:select", self.value.clone());
        if prevented {
            return;
        }
        if let Some(root) = inject(&ROOT) {
            root.update(|r: &mut PineCommandRoot| r.close());
        }
    }
}

// ── Empty ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineCommandEmpty.poco", role = "panel", display = "contents")]
pub struct PineCommandEmpty {
    pub empty: bool,
}

#[handlers]
impl PineCommandEmpty {
    pub fn on_setup(&mut self) {
        if let Some(root) = inject(&ROOT) {
            self.empty = root.with(|r| !r.has_matches);
        }
    }

    pub fn on_ready(&self, handle: pocopine::Handle<Self>) {
        let Some(root) = inject::<Handle<PineCommandRoot>>(&ROOT) else {
            return;
        };
        watch_scope_field::<bool, _>(root.scope_id(), "has_matches", move |&v, _| {
            handle.update(|s| s.empty = !v);
        });
    }
}
