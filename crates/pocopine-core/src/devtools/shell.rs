//! Outer panel frame: root `<div>`, header, meta line, body host.
//!
//! The shell owns only the *chrome* around the active panel — title,
//! inspect button, collapse button, close button, meta line ("N
//! scopes"), and the body host into which the active panel's content
//! is painted. Mode toggle (tree/flat) used to live here; as of PR A
//! it's moved into the scope panel's own sub-header since it's
//! scope-inspector-specific.
//!
//! The shell builds its DOM once (lazy on first render) and then
//! only updates specific sub-elements in place — inspect button
//! state, collapse attribute, meta text — so per-tick work stays
//! minimal when nothing changed.

use std::cell::Cell;

use wasm_bindgen::prelude::*;
use web_sys::{window, Document, Element, HtmlElement};

use super::{event, panel};
use crate::scope::Scope;

pub(super) const ROOT_ID: &str = "__pp_devtools_root";
pub(super) const META_ID: &str = "__pp_dev_meta";
pub(super) const INSPECT_BTN_ID: &str = "__pp_dev_btn_inspect";
pub(super) const BODY_ID: &str = "__pp_dev_body";
pub(super) const TABS_ID: &str = "__pp_dev_tabs";

thread_local! {
    static VISIBLE: Cell<bool> = const { Cell::new(true) };
    static SHELL_BUILT: Cell<bool> = const { Cell::new(false) };
    static COLLAPSED: Cell<bool> = const { Cell::new(false) };

    static LAST_META: std::cell::RefCell<String> =
        const { std::cell::RefCell::new(String::new()) };
    static LAST_INSPECT_RENDERED: Cell<bool> = const { Cell::new(false) };
    static LAST_COLLAPSED_RENDERED: Cell<Option<bool>> = const { Cell::new(None) };
    static LAST_TABS_FP: std::cell::RefCell<String> =
        const { std::cell::RefCell::new(String::new()) };
}

pub(super) fn is_visible() -> bool {
    VISIBLE.with(|c| c.get())
}

pub(super) fn set_visible(v: bool) {
    VISIBLE.with(|c| c.set(v));
}

pub(super) fn toggle_collapse() {
    COLLAPSED.with(|c| c.set(!c.get()));
}

pub(super) fn is_collapsed() -> bool {
    COLLAPSED.with(|c| c.get())
}

/// Resolve the panel root from `document.getElementById` without
/// creating one. Used by inspect + event helpers that need the root
/// for containment checks but shouldn't side-effect the DOM.
pub(super) fn panel_root() -> Option<Element> {
    window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(ROOT_ID))
}

/// Resolve or create the panel root. On first call this appends a
/// `<div id="__pp_devtools_root">` to `<body>` and attaches the
/// delegated click + hover listeners. Subsequent calls return the
/// existing element.
pub(super) fn ensure_root(doc: &Document) -> Element {
    if let Some(el) = doc.get_element_by_id(ROOT_ID) {
        return el;
    }
    let el = doc.create_element("div").expect("create_element div");
    let _ = el.set_attribute("id", ROOT_ID);
    if let Some(body) = doc.body() {
        let _ = body.append_child(&el);
    }
    event::attach(&el);
    el
}

/// Lazy one-time build of the shell skeleton. Header + meta line +
/// body host. The body host contains the per-panel host divs, built
/// when each panel mounts.
pub(super) fn build_shell_once(root: &Element) {
    if SHELL_BUILT.with(|c| c.get()) {
        return;
    }
    let shell = format!(
        "<header class=\"__pp_dev_header\">\
           <span>pocopine devtools</span>\
           <div class=\"__pp_dev_actions\">\
             <button id=\"{btn}\" class=\"__pp_dev_btn\" data-action=\"toggle-inspect\">\
               inspect\
             </button>\
             <button class=\"__pp_dev_btn __pp_dev_collapse\" data-action=\"toggle-collapse\" \
                     title=\"collapse\">−</button>\
             <button class=\"__pp_dev_btn __pp_dev_close\" data-action=\"close\">×</button>\
           </div>\
         </header>\
         <div id=\"{tabs}\" class=\"__pp_dev_tabs\" style=\"display:none\"></div>\
         <div id=\"{meta}\" class=\"__pp_dev_meta\"></div>\
         <div id=\"{body}\" class=\"__pp_dev_body\"></div>",
        btn = INSPECT_BTN_ID,
        tabs = TABS_ID,
        meta = META_ID,
        body = BODY_ID,
    );
    root.set_inner_html(&shell);
    SHELL_BUILT.with(|c| c.set(true));
}

/// Sync the collapsed attribute / button label when state changes.
pub(super) fn update_collapse_state(root: &Element) {
    let collapsed = is_collapsed();
    let prev = LAST_COLLAPSED_RENDERED.with(|c| c.get());
    if prev == Some(collapsed) {
        return;
    }
    if collapsed {
        let _ = root.set_attribute("data-collapsed", "true");
    } else {
        let _ = root.remove_attribute("data-collapsed");
    }
    if let Some(btn) = root.query_selector(".__pp_dev_collapse").ok().flatten() {
        btn.set_text_content(Some(if collapsed { "+" } else { "−" }));
        let _ = btn.set_attribute(
            "title",
            if collapsed { "expand" } else { "collapse" },
        );
    }
    LAST_COLLAPSED_RENDERED.with(|c| c.set(Some(collapsed)));
}

/// Sync the inspect-mode button appearance.
pub(super) fn update_inspect_button(root: &Element) {
    let on = super::inspect::is_on();
    let prev = LAST_INSPECT_RENDERED.with(|c| c.get());
    if on == prev {
        return;
    }
    if let Some(btn) = root.query_selector(&format!("#{INSPECT_BTN_ID}")).ok().flatten() {
        if on {
            let _ = btn.set_attribute("class", "__pp_dev_btn __pp_dev_btn_on");
            btn.set_text_content(Some("inspecting…"));
        } else {
            let _ = btn.set_attribute("class", "__pp_dev_btn");
            btn.set_text_content(Some("inspect"));
        }
    }
    LAST_INSPECT_RENDERED.with(|c| c.set(on));
}

/// Rewrite the meta line when the scope count changes.
pub(super) fn update_meta_line(root: &Element, scopes: &[Scope]) {
    let meta_text = format!("{} scopes", scopes.len());
    let needs_write = LAST_META.with(|c| {
        let mut cur = c.borrow_mut();
        if *cur != meta_text {
            *cur = meta_text.clone();
            true
        } else {
            false
        }
    });
    if needs_write {
        if let Some(meta) = root.query_selector(&format!("#{META_ID}")).ok().flatten() {
            meta.set_text_content(Some(&meta_text));
        }
    }
}

/// Paint the tab strip when ≥2 panels are registered; hide it when
/// 1. Fingerprint is `"<count>|<active>"` so we skip rewrites while
/// nothing about the strip changes.
pub(super) fn update_tab_strip(root: &Element) {
    let summary = panel::summary();
    if summary.len() < 2 {
        if let Some(tabs) = root.query_selector(&format!("#{TABS_ID}")).ok().flatten() {
            if let Ok(html_el) = tabs.dyn_into::<HtmlElement>() {
                let _ = html_el.style().set_property("display", "none");
            }
        }
        return;
    }
    let fp = summary
        .iter()
        .map(|(id, _, active)| format!("{id}:{active}"))
        .collect::<Vec<_>>()
        .join(";");
    let changed = LAST_TABS_FP.with(|c| {
        let mut cur = c.borrow_mut();
        if *cur != fp {
            *cur = fp;
            true
        } else {
            false
        }
    });
    if !changed {
        return;
    }
    let Some(tabs) = root.query_selector(&format!("#{TABS_ID}")).ok().flatten() else { return };
    if let Ok(html_el) = tabs.clone().dyn_into::<HtmlElement>() {
        let _ = html_el.style().remove_property("display");
    }
    let mut html = String::new();
    for (idx, (id, label, active)) in summary.iter().enumerate() {
        let cls = if *active {
            "__pp_dev_seg_btn __pp_dev_seg_btn_on"
        } else {
            "__pp_dev_seg_btn"
        };
        html.push_str(&format!(
            "<button class=\"{cls}\" data-action=\"select-panel\" \
                     data-panel-idx=\"{idx}\" data-panel-id=\"{id}\">{label}</button>"
        ));
    }
    tabs.set_inner_html(&format!("<div class=\"__pp_dev_seg\">{html}</div>"));
}

/// Resolve (or lazily create) the host `<div>` a panel paints into.
/// Each panel gets its own host under the body so the shell can
/// hide inactive panels via `display:none` without blowing away
/// their state.
pub(super) fn host_for_panel(doc: &Document, idx: usize, panel_id: &str) -> Option<Element> {
    let body = doc.get_element_by_id(BODY_ID)?;
    let host_id = format!("__pp_dev_panel_{panel_id}");
    if let Some(el) = doc.get_element_by_id(&host_id) {
        update_host_visibility(&el, idx);
        return Some(el);
    }
    let el = doc.create_element("div").ok()?;
    let _ = el.set_attribute("id", &host_id);
    let _ = el.set_attribute("class", "__pp_dev_panel_host");
    // Inactive panels default hidden; active one shows.
    update_host_visibility(&el, idx);
    let _ = body.append_child(&el);
    Some(el)
}

fn update_host_visibility(el: &Element, idx: usize) {
    let active = panel::active_index();
    if let Ok(html) = el.clone().dyn_into::<HtmlElement>() {
        let style = html.style();
        if idx == active {
            let _ = style.remove_property("display");
        } else {
            let _ = style.set_property("display", "none");
        }
    }
}

/// Re-apply `display: none / ""` to every registered panel's host
/// based on the current active index. Called every render tick so
/// tab switches take effect — `host_for_panel` sets the display
/// only on mount / lookup, and `ensure_mounted` short-circuits
/// once every panel is mounted, which left visibility frozen at
/// the first mount. This is the explicit sync path.
pub(super) fn sync_panel_host_visibility(doc: &Document) {
    let summary = panel::summary();
    for (idx, (id, _, _)) in summary.iter().enumerate() {
        let host_id = format!("__pp_dev_panel_{id}");
        if let Some(el) = doc.get_element_by_id(&host_id) {
            update_host_visibility(&el, idx);
        }
    }
}
