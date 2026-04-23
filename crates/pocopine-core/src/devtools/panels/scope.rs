//! Scope inspector panel — the default view.
//!
//! Two sub-panes inside the panel host:
//!   - left: tree of live scopes (tree-by-DOM-nesting or flat-by-id),
//!     with a tree/flat segmented toggle above it.
//!   - right: detail of the selected scope — keys, values, refs, and
//!     a collapsible JSON tree of state.
//!
//! Ported verbatim from the pre-split monolithic devtools with the
//! following shape changes:
//!   * the tree/flat toggle now lives inside the panel's own
//!     sub-header instead of the shell header;
//!   * the panel owns its own `SELECTED`, `VIEW_MODE`, and per-pane
//!     fingerprint caches;
//!   * all HTML strings are escaped / stringified via the shared
//!     helpers in `super::super::util` — not duplicated in this file.

use std::cell::Cell;

use js_sys::Object;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{window, Element};

use crate::reactive::ScopeId;
use crate::refs;
use crate::scope::Scope;

use super::super::panel::Panel;
use super::super::shell;
use super::super::util;

const TREE_ID: &str = "__pp_dev_tree";
const DETAIL_ID: &str = "__pp_dev_detail";
const SUB_HEADER_ID: &str = "__pp_dev_scope_subhd";

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Tree,
    Flat,
}

thread_local! {
    static VIEW_MODE: Cell<ViewMode> = const { Cell::new(ViewMode::Tree) };
    static SELECTED: Cell<Option<ScopeId>> = const { Cell::new(None) };
    static LAST_MODE_RENDERED: Cell<Option<ViewMode>> = const { Cell::new(None) };
}

pub(crate) struct ScopeInspector;

impl Panel for ScopeInspector {
    fn id(&self) -> &'static str {
        "scope"
    }

    fn label(&self) -> &'static str {
        "Scopes"
    }

    fn mount(&self, host: &Element) {
        let html = format!(
            "<div id=\"{sub}\" class=\"__pp_dev_scope_subhd\">\
               <div class=\"__pp_dev_seg\">\
                 <button class=\"__pp_dev_seg_btn __pp_dev_seg_tree\" \
                         data-action=\"set-mode\" data-mode=\"tree\">tree</button>\
                 <button class=\"__pp_dev_seg_btn __pp_dev_seg_flat\" \
                         data-action=\"set-mode\" data-mode=\"flat\">flat</button>\
               </div>\
             </div>\
             <div class=\"__pp_dev_panes\">\
               <div id=\"{tree}\" class=\"__pp_dev_tree\"></div>\
               <div id=\"{detail}\" class=\"__pp_dev_detail\"></div>\
             </div>",
            sub = SUB_HEADER_ID,
            tree = TREE_ID,
            detail = DETAIL_ID,
        );
        host.set_inner_html(&html);
    }

    fn fingerprint(&self) -> String {
        let scopes = Scope::all();
        let mode = VIEW_MODE.with(|c| c.get());
        let selected = pick_selected(&scopes);
        let rows = build_rows(mode, &scopes);
        let tree_fp = tree_fingerprint(mode, &rows, selected, &scopes);
        let detail_fp = detail_fingerprint(selected, &scopes);
        format!("{tree_fp}//{detail_fp}")
    }

    fn render(&self, host: &Element) {
        let scopes = Scope::all();
        let mode = VIEW_MODE.with(|c| c.get());
        let selected = pick_selected(&scopes);

        // Mode buttons — painted in the sub-header.
        let prev_mode = LAST_MODE_RENDERED.with(|c| c.get());
        if prev_mode != Some(mode) {
            update_mode_buttons(host, mode);
            LAST_MODE_RENDERED.with(|c| c.set(Some(mode)));
        }

        let rows = build_rows(mode, &scopes);

        // Tree pane.
        if let Some(container) = host.query_selector(&format!("#{TREE_ID}")).ok().flatten() {
            container.set_inner_html(&build_tree_html(&rows, selected, &scopes));
        }

        // Detail pane.
        if let Some(detail) = host.query_selector(&format!("#{DETAIL_ID}")).ok().flatten() {
            let html = selected
                .and_then(|id| scopes.iter().find(|s| s.id == id).cloned())
                .map(|s| build_detail_html(&s))
                .unwrap_or_else(build_empty_detail_html);
            detail.set_inner_html(&html);
        }
    }

    fn handle_action(&self, action: &str, el: &Element) -> bool {
        match action {
            "set-mode" => {
                if let Some(m) = el.get_attribute("data-mode") {
                    let mode = match m.as_str() {
                        "flat" => ViewMode::Flat,
                        _ => ViewMode::Tree,
                    };
                    VIEW_MODE.with(|c| c.set(mode));
                    super::super::panel::invalidate_active();
                }
                true
            }
            "select-scope" => {
                if let Some(id_str) = el.get_attribute("data-scope-id") {
                    if let Ok(n) = id_str.parse::<u64>() {
                        SELECTED.with(|c| c.set(Some(ScopeId(n))));
                        super::super::panel::invalidate_active();
                    }
                }
                true
            }
            _ => false,
        }
    }
}

/// Called by the inspect picker when the user clicks an element on
/// the page. Sets the selection directly so the next render lights
/// up the matching tree row. Invalidates the panel's fingerprint so
/// the shell re-renders on the following tick.
pub(in crate::devtools) fn select(id: ScopeId) {
    SELECTED.with(|c| c.set(Some(id)));
    super::super::panel::invalidate_active();
}

/// The currently-selected scope id, if any. Exposed so sibling
/// panels (router/inject chain) can pivot on the user's
/// "current scope" without duplicating selection state.
pub(in crate::devtools) fn current_selection() -> Option<ScopeId> {
    SELECTED.with(|c| c.get())
}

// ── row assembly ──────────────────────────────────────────────────

fn pick_selected(scopes: &[Scope]) -> Option<ScopeId> {
    let cur = SELECTED.with(|c| c.get());
    let still_alive = cur.is_some_and(|id| scopes.iter().any(|s| s.id == id));
    if still_alive {
        return cur;
    }
    let next = scopes.first().map(|s| s.id);
    SELECTED.with(|c| c.set(next));
    next
}

fn build_rows(mode: ViewMode, scopes: &[Scope]) -> Vec<(ScopeId, usize)> {
    match mode {
        ViewMode::Tree => build_tree_rows(scopes),
        ViewMode::Flat => scopes.iter().map(|s| (s.id, 0usize)).collect(),
    }
}

/// Walk the DOM once, emitting `(scope_id, depth)` tuples for every
/// element that owns a non-borrowed scope. Borrowed scopes
/// (pp-teleport, pp-if's teleport path) are skipped — the real scope
/// is counted at the element that owns it. "Orphan" scopes (stores,
/// or scopes whose element lives inside the devtools panel itself)
/// are appended at the end at depth 0.
fn build_tree_rows(scopes: &[Scope]) -> Vec<(ScopeId, usize)> {
    let mut out: Vec<(ScopeId, usize)> = Vec::new();
    let Some(doc) = window().and_then(|w| w.document()) else {
        return out;
    };
    let Some(body) = doc.body() else { return out };
    let body_el: Element = body.into();
    let mut depth: usize = 0;
    walk_tree(&body_el, &mut depth, &mut out);

    let seen: std::collections::HashSet<ScopeId> = out.iter().map(|(id, _)| *id).collect();
    for s in scopes {
        if !seen.contains(&s.id) {
            out.push((s.id, 0));
        }
    }
    out
}

fn walk_tree(el: &Element, depth: &mut usize, out: &mut Vec<(ScopeId, usize)>) {
    // Skip the panel's own subtree.
    if el.id() == shell::ROOT_ID {
        return;
    }
    let scope_id = element_scope_id(el);
    if let Some(id) = scope_id {
        out.push((id, *depth));
        *depth += 1;
    }
    let children = el.children();
    for i in 0..children.length() {
        if let Some(c) = children.item(i) {
            walk_tree(&c, depth, out);
        }
    }
    if scope_id.is_some() {
        *depth -= 1;
    }
}

/// Read the non-borrowed scope id pinned on `el`, if any.
fn element_scope_id(el: &Element) -> Option<ScopeId> {
    let id_num = js_sys::Reflect::get(el.as_ref(), &"__pp_scope_id".into())
        .ok()?
        .as_f64()?;
    let borrowed = js_sys::Reflect::get(el.as_ref(), &"__pp_scope_borrowed".into())
        .ok()
        .map(|v| v.is_truthy())
        .unwrap_or(false);
    if borrowed {
        return None;
    }
    Some(ScopeId(id_num as u64))
}

// ── fingerprints ──────────────────────────────────────────────────

fn tree_fingerprint(
    mode: ViewMode,
    rows: &[(ScopeId, usize)],
    selected: Option<ScopeId>,
    scopes: &[Scope],
) -> String {
    let mut out = String::new();
    out.push(match mode {
        ViewMode::Tree => 't',
        ViewMode::Flat => 'f',
    });
    out.push('|');
    if let Some(id) = selected {
        out.push_str(&id.0.to_string());
    }
    out.push('|');
    for (id, depth) in rows {
        let tag = scopes
            .iter()
            .find(|s| s.id == *id)
            .map(|s| s.state.borrow().type_name().to_string())
            .unwrap_or_else(|| "?".into());
        out.push_str(&format!("{}:{}:{};", id.0, depth, tag));
    }
    out
}

fn detail_fingerprint(selected: Option<ScopeId>, scopes: &[Scope]) -> String {
    match selected {
        Some(id) => match scopes.iter().find(|s| s.id == id) {
            Some(scope) => format!("{}|{}", id.0, section_fingerprint(scope)),
            None => String::new(),
        },
        None => String::new(),
    }
}

/// Canonical per-scope string used to decide whether to rewrite the
/// detail pane. Covers tag, every declared field's raw value, and
/// the ordered ref names registered against this scope.
fn section_fingerprint(scope: &Scope) -> String {
    let mut out = String::new();
    {
        let state = scope.state.borrow();
        out.push_str(state.type_name());
        out.push('\u{1}');
        for key in state.keys() {
            let v = state.get(key);
            out.push_str(key);
            out.push('=');
            out.push_str(&util::raw_value(&v));
            out.push('\u{1}');
        }
    }
    let refs_obj = refs::as_object(scope.id);
    if let Ok(refs_obj) = refs_obj.dyn_into::<Object>() {
        let keys = Object::keys(&refs_obj);
        out.push_str("refs=");
        for i in 0..keys.length() {
            if let Some(name) = keys.get(i).as_string() {
                out.push_str(&name);
                out.push(',');
            }
        }
    }
    out
}

// ── HTML builders ─────────────────────────────────────────────────

fn update_mode_buttons(host: &Element, mode: ViewMode) {
    let mark = |cls: &str, active: bool| {
        if let Some(btn) = host.query_selector(cls).ok().flatten() {
            let base = "__pp_dev_seg_btn";
            let active_cls = "__pp_dev_seg_btn_on";
            let specifics = cls.trim_start_matches('.');
            let mut classes = format!("{base} {specifics}");
            if active {
                classes.push(' ');
                classes.push_str(active_cls);
            }
            let _ = btn.set_attribute("class", &classes);
        }
    };
    mark(".__pp_dev_seg_tree", matches!(mode, ViewMode::Tree));
    mark(".__pp_dev_seg_flat", matches!(mode, ViewMode::Flat));
}

fn build_tree_html(
    rows: &[(ScopeId, usize)],
    selected: Option<ScopeId>,
    scopes: &[Scope],
) -> String {
    if rows.is_empty() {
        return r#"<div class="__pp_dev_empty">no live scopes</div>"#.into();
    }
    let mut html = String::new();
    for (id, depth) in rows {
        let tag = scopes
            .iter()
            .find(|s| s.id == *id)
            .map(|s| s.state.borrow().type_name().to_string())
            .unwrap_or_else(|| "?".into());
        let is_selected = selected == Some(*id);
        let selected_cls = if is_selected {
            " __pp_dev_tree_node_on"
        } else {
            ""
        };
        html.push_str(&format!(
            "<div class=\"__pp_dev_tree_node{sel}\" \
                  data-action=\"select-scope\" \
                  data-scope-id=\"{id}\" \
                  style=\"padding-left: {pad}px\">\
               <span class=\"__pp_dev_tag\">&lt;{tag}&gt;</span>\
               <span class=\"__pp_dev_id\">#{id}</span>\
             </div>",
            sel = selected_cls,
            id = id.0,
            pad = 6 + depth * 12,
            tag = util::escape(&tag),
        ));
    }
    html
}

fn build_empty_detail_html() -> String {
    r#"<div class="__pp_dev_empty __pp_dev_detail_empty">no scope selected</div>"#.into()
}

fn build_detail_html(scope: &Scope) -> String {
    let state = scope.state.borrow();
    let tag = util::escape(state.type_name());
    let keys = state.keys();

    let mut html = format!(
        "<section class=\"__pp_dev_scope\" data-scope-id=\"{id}\">\
           <div class=\"__pp_dev_scope_hd\">\
             <span class=\"__pp_dev_tag\">&lt;{tag}&gt;</span>\
             <span class=\"__pp_dev_id\">#{id}</span>\
           </div>\
           <dl class=\"__pp_dev_kv\">",
        id = scope.id.0,
    );
    if keys.is_empty() {
        html.push_str("<div class=\"__pp_dev_empty\">no declared fields</div>");
    } else {
        for key in keys {
            let v = state.get(key);
            let display = util::stringify(&v);
            let full = util::raw_value(&v);
            html.push_str(&format!(
                "<div class=\"__pp_dev_row\">\
                   <dt>{k}</dt>\
                   <dd data-copy=\"{full}\" title=\"click to copy\">{display}</dd>\
                 </div>",
                k = util::escape(key),
                full = util::escape(&full),
                display = util::escape(&display),
            ));
        }
    }

    // Assemble the state as a JS object so we can render it as a
    // tree and (later) let the user edit leaf values.
    let payload = Object::new();
    for key in state.keys() {
        let v = state.get(key);
        let _ = js_sys::Reflect::set(&payload, &JsValue::from_str(key), &v);
    }
    let json_tree = util::build_json_view(&payload.into(), true);

    drop(state);

    // Refs registered against this scope.
    let refs_obj = refs::as_object(scope.id);
    if let Ok(refs_obj) = refs_obj.dyn_into::<Object>() {
        let ref_keys = Object::keys(&refs_obj);
        if ref_keys.length() > 0 {
            html.push_str("<div class=\"__pp_dev_refs\"><span>refs:</span> ");
            for i in 0..ref_keys.length() {
                if i > 0 {
                    html.push_str(", ");
                }
                if let Some(name) = ref_keys.get(i).as_string() {
                    html.push_str(&format!("<code>{}</code>", util::escape(&name)));
                }
            }
            html.push_str("</div>");
        }
    }
    html.push_str("</dl>");

    html.push_str(&format!(
        "<div class=\"__pp_dev_json\">\
           <div class=\"__pp_dev_json_label\">state</div>\
           <div class=\"__pp_jv\">{json_tree}</div>\
         </div>"
    ));
    html.push_str("</section>");

    html
}
