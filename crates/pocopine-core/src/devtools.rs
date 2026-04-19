//! pocopine devtools — a floating overlay that lists live scopes, their
//! current state, and registered refs. Opt-in via [`App::with_devtools`].
//!
//! Activation:
//! - Opt-in at startup: `App::new().with_devtools().run()`.
//! - Toggle visibility with `Ctrl+Shift+D`.
//!
//! Rendering model: a `setInterval(200ms)` snapshot, not a reactive
//! subscription. A dev panel doesn't need sub-frame latency, and the
//! snapshot approach keeps the reactive core untouched.

use std::cell::{Cell, RefCell};

use js_sys::Object;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use web_sys::{window, Document, Element, Event, HtmlElement, KeyboardEvent, Node};

use crate::reactive::ScopeId;

use crate::refs;
use crate::scope::Scope;

const STYLE_ID: &str = "__pp_devtools_style";
const ROOT_ID: &str = "__pp_devtools_root";
const HIGHLIGHT_CLASS: &str = "__pp_dev_highlight";
const TREE_ID: &str = "__pp_dev_tree";
const DETAIL_ID: &str = "__pp_dev_detail";
const META_ID: &str = "__pp_dev_meta";
const INSPECT_BTN_ID: &str = "__pp_dev_btn_inspect";

type RenderClosure = Closure<dyn FnMut()>;
type KeyClosure = Closure<dyn FnMut(KeyboardEvent)>;
type EventClosure = Closure<dyn FnMut(Event)>;

/// Scope list layout. Toggle via the tree/flat segmented control in
/// the panel header.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    /// Mirror the DOM hierarchy — a scope is indented under the
    /// nearest ancestor element that also owns a scope.
    Tree,
    /// All scopes at depth 0, sorted by id. Original flat layout.
    Flat,
}

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    static VISIBLE: Cell<bool> = const { Cell::new(true) };
    static INSPECT_MODE: Cell<bool> = const { Cell::new(false) };
    static INTERVAL_ID: Cell<Option<i32>> = const { Cell::new(None) };
    static RENDER_CB: RefCell<Option<RenderClosure>> = RefCell::new(None);
    static KEY_CB: RefCell<Option<KeyClosure>> = RefCell::new(None);
    static CLICK_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static OVER_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static LEAVE_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static DOC_OVER_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static DOC_CLICK_CB: RefCell<Option<EventClosure>> = RefCell::new(None);
    static HIGHLIGHTED: RefCell<Option<Element>> = const { RefCell::new(None) };

    // Incremental render state — persistent across ticks.
    static SHELL_BUILT: Cell<bool> = const { Cell::new(false) };
    static LAST_META: RefCell<String> = const { RefCell::new(String::new()) };
    static LAST_INSPECT_RENDERED: Cell<bool> = const { Cell::new(false) };
    static LAST_MODE_RENDERED: Cell<Option<ViewMode>> = const { Cell::new(None) };
    static LAST_TREE_FP: RefCell<String> = const { RefCell::new(String::new()) };
    static LAST_DETAIL_FP: RefCell<String> = const { RefCell::new(String::new()) };
    static VIEW_MODE: Cell<ViewMode> = const { Cell::new(ViewMode::Tree) };
    static SELECTED: Cell<Option<ScopeId>> = const { Cell::new(None) };
}

/// Install the devtools overlay. Idempotent — calling twice is a
/// no-op. Wires up the `Ctrl+Shift+D` toggle and starts the render
/// loop. Safe to call before `walker::start` — we lazy-attach the
/// panel to `<body>` on the first render.
pub fn install() {
    if INSTALLED.with(|c| c.get()) {
        return;
    }
    INSTALLED.with(|c| c.set(true));

    inject_stylesheet();
    attach_key_listener();
    start_render_loop();
}

fn inject_stylesheet() {
    let Some(doc) = window().and_then(|w| w.document()) else { return };
    if doc.get_element_by_id(STYLE_ID).is_some() {
        return;
    }
    let Ok(style) = doc.create_element("style") else { return };
    let _ = style.set_attribute("id", STYLE_ID);
    style.set_text_content(Some(STYLESHEET));
    if let Some(head) = doc.head() {
        let _ = head.append_child(&style);
    }
}

fn attach_key_listener() {
    let Some(win) = window() else { return };
    let cb: Closure<dyn FnMut(KeyboardEvent)> =
        Closure::wrap(Box::new(move |ev: KeyboardEvent| {
            if ev.ctrl_key() && ev.shift_key() && ev.key().eq_ignore_ascii_case("d") {
                ev.prevent_default();
                toggle();
            }
        }));
    let _ = win.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
    KEY_CB.with(|c| *c.borrow_mut() = Some(cb));
}

fn start_render_loop() {
    let Some(win) = window() else { return };
    let cb: Closure<dyn FnMut()> =
        Closure::wrap(Box::new(move || {
            render();
        }));
    if let Ok(id) = win.set_interval_with_callback_and_timeout_and_arguments_0(
        cb.as_ref().unchecked_ref(),
        200,
    ) {
        INTERVAL_ID.with(|c| c.set(Some(id)));
    }
    RENDER_CB.with(|c| *c.borrow_mut() = Some(cb));
    // Also fire one immediate render so the panel appears right away.
    render();
}

/// Toggle overlay visibility. Bound to `Ctrl+Shift+D` at install time.
pub fn toggle() {
    let next = !VISIBLE.with(|c| c.get());
    VISIBLE.with(|c| c.set(next));
    if let Some(root) = panel_root() {
        if let Ok(html_el) = root.clone().dyn_into::<HtmlElement>() {
            let style = html_el.style();
            let _ = style.set_property("display", if next { "block" } else { "none" });
        }
    }
}

fn render() {
    if !INSTALLED.with(|c| c.get()) {
        return;
    }
    let Some(doc) = window().and_then(|w| w.document()) else { return };
    let root = ensure_root(&doc);
    let Ok(html_el) = root.clone().dyn_into::<HtmlElement>() else { return };
    if !VISIBLE.with(|c| c.get()) {
        let _ = html_el.style().set_property("display", "none");
        return;
    } else {
        let _ = html_el.style().remove_property("display");
    }

    build_shell_once(&root);

    // Header — inspect button reflects INSPECT_MODE; mode toggle
    // reflects VIEW_MODE.
    let inspect_on = INSPECT_MODE.with(|c| c.get());
    let prev_inspect = LAST_INSPECT_RENDERED.with(|c| c.get());
    if inspect_on != prev_inspect {
        update_inspect_button(&root, inspect_on);
        LAST_INSPECT_RENDERED.with(|c| c.set(inspect_on));
    }
    let mode = VIEW_MODE.with(|c| c.get());
    let prev_mode = LAST_MODE_RENDERED.with(|c| c.get());
    if prev_mode != Some(mode) {
        update_mode_buttons(&root, mode);
        LAST_MODE_RENDERED.with(|c| c.set(Some(mode)));
    }

    let scopes = Scope::all();

    // Meta line — rewrites only when the string changes.
    let meta_text = format!("{} scopes", scopes.len());
    let needs_meta_write = LAST_META.with(|c| {
        let mut cur = c.borrow_mut();
        if *cur != meta_text {
            *cur = meta_text.clone();
            true
        } else {
            false
        }
    });
    if needs_meta_write {
        if let Some(meta) = root.query_selector(&format!("#{META_ID}")).ok().flatten() {
            meta.set_text_content(Some(&meta_text));
        }
    }

    // Resolve the current selection: default to first scope; drop a
    // stale id if the scope it pointed at is gone.
    let selected = pick_selected(&scopes);

    // Build the ordered scope list the tree pane iterates. Tree mode
    // walks the DOM to derive `(id, depth)` tuples; flat mode just
    // emits every scope at depth 0, id-sorted.
    let rows = match mode {
        ViewMode::Tree => build_tree_rows(&scopes),
        ViewMode::Flat => scopes.iter().map(|s| (s.id, 0usize)).collect(),
    };

    // Tree pane. Fingerprint covers layout + selection + per-row tag
    // — regeneration only fires when the user-visible tree changes.
    let tree_fp = tree_fingerprint(mode, &rows, selected, &scopes);
    let needs_tree_write = LAST_TREE_FP.with(|c| {
        let mut cur = c.borrow_mut();
        if *cur != tree_fp {
            *cur = tree_fp.clone();
            true
        } else {
            false
        }
    });
    if needs_tree_write {
        if let Some(container) = root.query_selector(&format!("#{TREE_ID}")).ok().flatten() {
            container.set_inner_html(&build_tree_html(&rows, selected, &scopes));
        }
    }

    // Detail pane. Fingerprint covers selected id + its section
    // fingerprint, so editing a single field only rewrites this one
    // pane.
    let detail_fp = detail_fingerprint(selected, &scopes);
    let needs_detail_write = LAST_DETAIL_FP.with(|c| {
        let mut cur = c.borrow_mut();
        if *cur != detail_fp {
            *cur = detail_fp.clone();
            true
        } else {
            false
        }
    });
    if needs_detail_write {
        if let Some(detail) = root.query_selector(&format!("#{DETAIL_ID}")).ok().flatten() {
            let html = selected
                .and_then(|id| scopes.iter().find(|s| s.id == id).cloned())
                .map(|s| build_detail_html(&s))
                .unwrap_or_else(build_empty_detail_html);
            detail.set_inner_html(&html);
        }
    }
}

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

/// Build the panel's static shell once. Subsequent renders update
/// specific sub-elements in place.
fn build_shell_once(root: &Element) {
    if SHELL_BUILT.with(|c| c.get()) {
        return;
    }
    let shell = format!(
        "<header class=\"__pp_dev_header\">\
           <span>pocopine devtools</span>\
           <div class=\"__pp_dev_actions\">\
             <div class=\"__pp_dev_seg\">\
               <button class=\"__pp_dev_seg_btn __pp_dev_seg_tree\" \
                 data-action=\"set-mode\" data-mode=\"tree\">tree</button>\
               <button class=\"__pp_dev_seg_btn __pp_dev_seg_flat\" \
                 data-action=\"set-mode\" data-mode=\"flat\">flat</button>\
             </div>\
             <button id=\"{btn}\" class=\"__pp_dev_btn\" data-action=\"toggle-inspect\">\
               inspect\
             </button>\
             <button class=\"__pp_dev_btn __pp_dev_close\" data-action=\"close\">×</button>\
           </div>\
         </header>\
         <div id=\"{meta}\" class=\"__pp_dev_meta\"></div>\
         <div class=\"__pp_dev_body\">\
           <div id=\"{tree}\" class=\"__pp_dev_tree\"></div>\
           <div id=\"{detail}\" class=\"__pp_dev_detail\"></div>\
         </div>",
        btn = INSPECT_BTN_ID,
        meta = META_ID,
        tree = TREE_ID,
        detail = DETAIL_ID,
    );
    root.set_inner_html(&shell);
    SHELL_BUILT.with(|c| c.set(true));
}

fn update_inspect_button(root: &Element, on: bool) {
    let Some(btn) = root.query_selector(&format!("#{INSPECT_BTN_ID}")).ok().flatten() else {
        return;
    };
    if on {
        let _ = btn.set_attribute("class", "__pp_dev_btn __pp_dev_btn_on");
        btn.set_text_content(Some("inspecting…"));
    } else {
        let _ = btn.set_attribute("class", "__pp_dev_btn");
        btn.set_text_content(Some("inspect"));
    }
}

fn update_mode_buttons(root: &Element, mode: ViewMode) {
    let mark = |cls: &str, active: bool| {
        if let Some(btn) = root.query_selector(cls).ok().flatten() {
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

/// Walk the DOM once, emitting `(scope_id, depth)` tuples for every
/// element that owns a non-borrowed scope. Borrowed scopes (pp-teleport,
/// pp-if's teleport path) are skipped — the real scope is counted at
/// the element that owns it.
fn build_tree_rows(scopes: &[Scope]) -> Vec<(ScopeId, usize)> {
    let mut out: Vec<(ScopeId, usize)> = Vec::new();
    let Some(doc) = window().and_then(|w| w.document()) else { return out };
    let Some(body) = doc.body() else { return out };
    let body_el: Element = body.into();
    let mut depth: usize = 0;
    walk_tree(&body_el, &mut depth, &mut out);

    // Append "orphan" scopes — stores, or scopes whose element lives
    // inside the devtools panel (excluded by `walk_tree`).
    let seen: std::collections::HashSet<ScopeId> =
        out.iter().map(|(id, _)| *id).collect();
    for s in scopes {
        if !seen.contains(&s.id) {
            out.push((s.id, 0));
        }
    }
    out
}

fn walk_tree(el: &Element, depth: &mut usize, out: &mut Vec<(ScopeId, usize)>) {
    // Skip the panel's own subtree.
    if el.id() == ROOT_ID {
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
            tag = escape(&tag),
        ));
    }
    html
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
            out.push_str(&raw_value(&v));
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

fn build_empty_detail_html() -> String {
    r#"<div class="__pp_dev_empty __pp_dev_detail_empty">no scope selected</div>"#.into()
}

fn build_detail_html(scope: &Scope) -> String {
    let state = scope.state.borrow();
    let tag = escape(state.type_name());
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
            let display = stringify(&v);
            let full = raw_value(&v);
            html.push_str(&format!(
                "<div class=\"__pp_dev_row\">\
                   <dt>{k}</dt>\
                   <dd data-copy=\"{full}\" title=\"click to copy\">{display}</dd>\
                 </div>",
                k = escape(key),
                full = escape(&full),
                display = escape(&display),
            ));
        }
    }

    // Full state as indented JSON — handy for large objects + a step
    // toward the editable-state follow-up.
    let mut payload = Object::new();
    {
        // `payload` is fine shadowed; the closure just below needs
        // a fresh object each time.
        for key in state.keys() {
            let v = state.get(key);
            let _ = js_sys::Reflect::set(&payload, &JsValue::from_str(key), &v);
        }
    }
    let json = js_sys::JSON::stringify_with_replacer_and_space(
        &payload,
        &JsValue::NULL,
        &JsValue::from_f64(2.0),
    )
    .ok()
    .and_then(|s| s.as_string())
    .unwrap_or_default();
    // reset payload binding for lint; real state is in `json` now
    payload = Object::new();
    let _ = &payload;

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
                    html.push_str(&format!(
                        "<code>{}</code>",
                        escape(&name)
                    ));
                }
            }
            html.push_str("</div>");
        }
    }
    html.push_str("</dl>");

    html.push_str(&format!(
        "<div class=\"__pp_dev_json\">\
           <div class=\"__pp_dev_json_label\">json</div>\
           <pre class=\"__pp_dev_json_content\" data-copy=\"{copy}\" \
                title=\"click to copy\">{pretty}</pre>\
         </div>",
        copy = escape(&json),
        pretty = escape(&json),
    ));
    html.push_str("</section>");

    html
}

fn ensure_root(doc: &Document) -> Element {
    if let Some(el) = doc.get_element_by_id(ROOT_ID) {
        return el;
    }
    let el = doc.create_element("div").expect("create_element div");
    let _ = el.set_attribute("id", ROOT_ID);
    if let Some(body) = doc.body() {
        let _ = body.append_child(&el);
    }
    attach_copy_delegate(&el);
    attach_hover_delegate(&el);
    el
}

/// Mouseover on the panel highlights the DOM element that owns the
/// scope the pointer is currently hovering. Mouseleave clears. Both
/// listeners stay on the root across `set_inner_html` rewrites.
fn attach_hover_delegate(root: &Element) {
    let over: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        let Some(target) = ev.target() else { return };
        let Ok(start) = target.dyn_into::<Element>() else { return };
        let mut cur = Some(start);
        while let Some(el) = cur {
            if let Some(id_str) = el.get_attribute("data-scope-id") {
                if let Ok(n) = id_str.parse::<u64>() {
                    set_highlight(Some(ScopeId(n)));
                    return;
                }
            }
            cur = el.parent_element();
        }
    }));
    let _ = root.add_event_listener_with_callback("mouseover", over.as_ref().unchecked_ref());
    OVER_CB.with(|c| *c.borrow_mut() = Some(over));

    let leave: EventClosure = Closure::wrap(Box::new(move |_ev: Event| {
        set_highlight(None);
    }));
    let _ = root.add_event_listener_with_callback("mouseleave", leave.as_ref().unchecked_ref());
    LEAVE_CB.with(|c| *c.borrow_mut() = Some(leave));
}

/// Apply the outline class to the element owning `scope_id`, or
/// clear it entirely when `None`. Idempotent across rapid pointer
/// moves — only writes the DOM when the highlighted element actually
/// changes.
fn set_highlight(scope_id: Option<ScopeId>) {
    let next = scope_id.and_then(crate::walker::find_element_for_scope);
    apply_highlight(next);
}

/// Lower-level: highlight exactly this element (inspect mode hovers
/// arbitrary DOM elements, not just component roots).
fn apply_highlight(next: Option<Element>) {
    let prev = HIGHLIGHTED.with(|h| h.borrow().clone());
    let same = match (&prev, &next) {
        (Some(a), Some(b)) => {
            let b_node: &Node = b.as_ref();
            a.is_same_node(Some(b_node))
        }
        (None, None) => true,
        _ => false,
    };
    if same {
        return;
    }
    if let Some(el) = prev {
        remove_highlight_class(&el);
    }
    if let Some(ref el) = next {
        add_highlight_class(el);
    }
    HIGHLIGHTED.with(|h| *h.borrow_mut() = next);
}

fn is_inside_panel(el: &Element) -> bool {
    let Some(root) = panel_root() else { return false };
    let root_node: &Node = root.as_ref();
    let el_node: &Node = el.as_ref();
    root_node.contains(Some(el_node))
}

/// Flip the "pick an element on the page" mode. When active, any
/// mouseover on the document outlines the element under the cursor
/// and a click picks it — the panel then scrolls to and flashes the
/// owning scope row, and inspect turns itself off.
fn toggle_inspect() {
    let next = !INSPECT_MODE.with(|c| c.get());
    INSPECT_MODE.with(|c| c.set(next));
    if next {
        attach_inspect_listeners();
    } else {
        detach_inspect_listeners();
        apply_highlight(None);
    }
}

fn attach_inspect_listeners() {
    let Some(doc) = window().and_then(|w| w.document()) else { return };

    let over: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        if !INSPECT_MODE.with(|c| c.get()) {
            return;
        }
        let Some(target) = ev.target() else { return };
        let Ok(el) = target.dyn_into::<Element>() else { return };
        if is_inside_panel(&el) {
            return;
        }
        apply_highlight(Some(el));
    }));
    let _ = doc.add_event_listener_with_callback("mouseover", over.as_ref().unchecked_ref());
    DOC_OVER_CB.with(|c| *c.borrow_mut() = Some(over));

    // Capture-phase click: we want to swallow the click before any
    // app handler runs while the user is picking.
    let click: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        if !INSPECT_MODE.with(|c| c.get()) {
            return;
        }
        let Some(target) = ev.target() else { return };
        let Ok(start) = target.dyn_into::<Element>() else { return };
        if is_inside_panel(&start) {
            return;
        }
        ev.prevent_default();
        ev.stop_propagation();

        if let Some((scope_id, _)) = crate::walker::enclosing_scope(&start) {
            scroll_panel_to_scope(scope_id);
            set_highlight(Some(scope_id));
        } else {
            apply_highlight(None);
        }

        INSPECT_MODE.with(|c| c.set(false));
        detach_inspect_listeners();
        render();
    }));
    let _ = doc.add_event_listener_with_callback_and_bool(
        "click",
        click.as_ref().unchecked_ref(),
        true,
    );
    DOC_CLICK_CB.with(|c| *c.borrow_mut() = Some(click));
}

fn detach_inspect_listeners() {
    let Some(doc) = window().and_then(|w| w.document()) else { return };
    if let Some(cb) = DOC_OVER_CB.with(|c| c.borrow_mut().take()) {
        let _ = doc.remove_event_listener_with_callback(
            "mouseover",
            cb.as_ref().unchecked_ref(),
        );
    }
    if let Some(cb) = DOC_CLICK_CB.with(|c| c.borrow_mut().take()) {
        let _ = doc.remove_event_listener_with_callback_and_bool(
            "click",
            cb.as_ref().unchecked_ref(),
            true,
        );
    }
}

fn scroll_panel_to_scope(scope_id: ScopeId) {
    let Some(root) = panel_root() else { return };
    let sel = format!("[data-scope-id=\"{}\"]", scope_id.0);
    if let Ok(Some(row)) = root.query_selector(&sel) {
        row.scroll_into_view();
        flash_copied(&row);
    }
}

fn add_highlight_class(el: &Element) {
    let cls = el.class_name();
    if !cls.split_whitespace().any(|c| c == HIGHLIGHT_CLASS) {
        el.set_class_name(format!("{cls} {HIGHLIGHT_CLASS}").trim());
    }
}

fn remove_highlight_class(el: &Element) {
    let kept: String = el
        .class_name()
        .split_whitespace()
        .filter(|c| *c != HIGHLIGHT_CLASS)
        .collect::<Vec<_>>()
        .join(" ");
    el.set_class_name(&kept);
}

/// One click listener on the root handles the whole panel via
/// delegation — the inner HTML gets blown away every render so
/// per-row listeners would leak fast. Any element with a
/// `data-copy="..."` attribute forwards its value to the clipboard.
fn attach_copy_delegate(root: &Element) {
    let cb: EventClosure = Closure::wrap(Box::new(move |ev: Event| {
        let Some(target) = ev.target() else { return };
        let Ok(start) = target.dyn_into::<Element>() else { return };
        let mut cur = Some(start);
        while let Some(el) = cur {
            if let Some(action) = el.get_attribute("data-action") {
                match action.as_str() {
                    "toggle-inspect" => {
                        toggle_inspect();
                        render();
                    }
                    "close" => toggle(),
                    "set-mode" => {
                        if let Some(m) = el.get_attribute("data-mode") {
                            let mode = match m.as_str() {
                                "flat" => ViewMode::Flat,
                                _ => ViewMode::Tree,
                            };
                            VIEW_MODE.with(|c| c.set(mode));
                            render();
                        }
                    }
                    "select-scope" => {
                        if let Some(id_str) = el.get_attribute("data-scope-id") {
                            if let Ok(n) = id_str.parse::<u64>() {
                                SELECTED.with(|c| c.set(Some(ScopeId(n))));
                                render();
                            }
                        }
                    }
                    _ => {}
                }
                return;
            }
            if let Some(value) = el.get_attribute("data-copy") {
                copy_to_clipboard(&value);
                flash_copied(&el);
                return;
            }
            cur = el.parent_element();
        }
    }));
    let _ = root.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
    CLICK_CB.with(|c| *c.borrow_mut() = Some(cb));
}

fn copy_to_clipboard(value: &str) {
    let Some(win) = window() else { return };
    let clipboard = win.navigator().clipboard();
    // Fire-and-forget; we don't await the promise.
    let _ = clipboard.write_text(value);
}

/// Briefly tag the clicked element so the user sees the copy
/// happened. The class is cleared by a one-shot `setTimeout`; the
/// next render (≤200ms) will also overwrite the node anyway.
fn flash_copied(el: &Element) {
    let cls = el.class_name();
    let needs_add = !cls.split_whitespace().any(|c| c == "__pp_dev_copied");
    if needs_add {
        el.set_class_name(format!("{cls} __pp_dev_copied").trim());
    }
    let el_for_reset = el.clone();
    let cb: Closure<dyn FnMut()> = Closure::once(Box::new(move || {
        let kept: String = el_for_reset
            .class_name()
            .split_whitespace()
            .filter(|c| *c != "__pp_dev_copied")
            .collect::<Vec<_>>()
            .join(" ");
        el_for_reset.set_class_name(&kept);
    }));
    if let Some(win) = window() {
        let _ = win.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            450,
        );
    }
    cb.forget();
}

fn panel_root() -> Option<Element> {
    window().and_then(|w| w.document()).and_then(|d| d.get_element_by_id(ROOT_ID))
}

/// Characters in a value cell before we replace the middle with `…`.
/// Chosen to roughly fit the 320px panel at the monospace size.
const MAX_VALUE_CHARS: usize = 80;

/// Best-effort string view of a `JsValue` for display in the panel.
/// Arrays collapse to a length summary; long strings / objects get
/// truncated in the middle so both ends stay visible.
fn stringify(v: &JsValue) -> String {
    if v.is_undefined() {
        return "undefined".into();
    }
    if v.is_null() {
        return "null".into();
    }
    if let Some(s) = v.as_string() {
        return truncate(&format!("\"{s}\""));
    }
    if let Some(n) = v.as_f64() {
        return n.to_string();
    }
    if let Some(b) = v.as_bool() {
        return b.to_string();
    }
    if js_sys::Array::is_array(v) {
        let len = js_sys::Array::from(v).length();
        return format!("[{len} items]");
    }
    let raw = js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .unwrap_or_else(|| "?".into());
    truncate(&raw)
}

/// The clipboard-friendly form of a `JsValue`. Strings come through
/// unquoted so paste is direct; everything else is its canonical JS /
/// JSON representation, not truncated.
fn raw_value(v: &JsValue) -> String {
    if v.is_undefined() {
        return "undefined".into();
    }
    if v.is_null() {
        return "null".into();
    }
    if let Some(s) = v.as_string() {
        return s;
    }
    if let Some(n) = v.as_f64() {
        return n.to_string();
    }
    if let Some(b) = v.as_bool() {
        return b.to_string();
    }
    js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .unwrap_or_default()
}

/// Clamp `s` to `MAX_VALUE_CHARS`, replacing the interior with `…` so
/// both the start and end stay readable. UTF-8 safe.
fn truncate(s: &str) -> String {
    let n = s.chars().count();
    if n <= MAX_VALUE_CHARS {
        return s.to_string();
    }
    let budget = MAX_VALUE_CHARS.saturating_sub(1);
    let head_len = budget.div_ceil(2);
    let tail_len = budget - head_len;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s.chars().rev().take(tail_len).collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

const STYLESHEET: &str = "\
    #__pp_devtools_root{position:fixed;top:12px;right:12px;width:540px;max-height:85vh;\
    overflow:hidden;z-index:2147483647;font-family:ui-monospace,Menlo,Consolas,monospace;\
    font-size:11px;line-height:1.45;color:#e6e1d8;background:#181715;border:1px solid #2d2a24;\
    border-radius:8px;box-shadow:0 10px 30px rgba(0,0,0,0.5);display:flex;flex-direction:column;}\
    #__pp_devtools_root *{box-sizing:border-box;}\
    .__pp_dev_header{display:flex;align-items:center;justify-content:space-between;\
    padding:8px 12px;background:#252220;border-bottom:1px solid #2d2a24;\
    color:#ff6600;letter-spacing:0.06em;text-transform:uppercase;font-size:10px;flex:0 0 auto;}\
    .__pp_dev_actions{display:flex;align-items:center;gap:6px;}\
    .__pp_dev_btn{background:none;border:1px solid transparent;color:#e6e1d8;font:inherit;\
    cursor:pointer;padding:2px 6px;border-radius:3px;letter-spacing:0.05em;\
    text-transform:uppercase;font-size:9px;}\
    .__pp_dev_btn:hover{border-color:#ff6600;color:#ff6600;}\
    .__pp_dev_btn_on{background:#ff6600;color:#181715;}\
    .__pp_dev_btn_on:hover{border-color:#ff6600;color:#181715;}\
    .__pp_dev_close{font-size:14px;padding:0 6px;}\
    .__pp_dev_seg{display:inline-flex;border:1px solid #2d2a24;border-radius:3px;overflow:hidden;}\
    .__pp_dev_seg_btn{background:none;border:none;color:#828282;font:inherit;\
    cursor:pointer;padding:2px 8px;letter-spacing:0.05em;text-transform:uppercase;font-size:9px;}\
    .__pp_dev_seg_btn:hover{color:#e6e1d8;}\
    .__pp_dev_seg_btn_on{background:#ff6600;color:#181715;}\
    .__pp_dev_seg_btn_on:hover{color:#181715;}\
    .__pp_dev_meta{padding:6px 12px;color:#828282;border-bottom:1px solid #2d2a24;flex:0 0 auto;}\
    .__pp_dev_body{display:flex;flex:1 1 auto;min-height:0;overflow:hidden;}\
    .__pp_dev_tree{flex:0 0 200px;overflow:auto;border-right:1px solid #2d2a24;padding:4px 0;}\
    .__pp_dev_tree_node{display:flex;justify-content:space-between;align-items:baseline;\
    padding:2px 10px 2px 6px;cursor:pointer;gap:6px;white-space:nowrap;}\
    .__pp_dev_tree_node:hover{background:#23211e;}\
    .__pp_dev_tree_node_on{background:#322a1d;}\
    .__pp_dev_tree_node_on:hover{background:#3a3021;}\
    .__pp_dev_detail{flex:1 1 auto;overflow:auto;padding:4px 8px 10px;}\
    .__pp_dev_detail_empty{padding:12px;}\
    .__pp_dev_scope{padding:6px 4px;}\
    .__pp_dev_scope_hd{display:flex;justify-content:space-between;align-items:baseline;\
    padding:2px 4px;}\
    .__pp_dev_tag{color:#ff6600;}\
    .__pp_dev_id{color:#828282;font-size:10px;}\
    .__pp_dev_kv{margin:6px 0 0;padding:0 4px;}\
    .__pp_dev_row{display:flex;gap:8px;padding:1px 0;}\
    .__pp_dev_row dt{color:#e6e1d8;flex:0 0 auto;min-width:7em;opacity:0.75;}\
    .__pp_dev_row dd{color:#c6e377;margin:0;word-break:break-all;cursor:pointer;}\
    .__pp_dev_row dd:hover{color:#fff;}\
    .__pp_dev_row dd.__pp_dev_copied{color:#181715;background:#ff6600;border-radius:2px;\
    padding:0 3px;}\
    .__pp_dev_empty{color:#828282;font-style:italic;padding:2px 4px;}\
    .__pp_dev_refs{margin-top:6px;color:#828282;font-size:10px;padding:2px 4px;}\
    .__pp_dev_refs code{color:#c6e377;}\
    .__pp_dev_json{margin-top:10px;border-top:1px dashed #2d2a24;padding-top:6px;}\
    .__pp_dev_json_label{color:#828282;font-size:9px;text-transform:uppercase;\
    letter-spacing:0.08em;margin-bottom:4px;padding:0 4px;}\
    .__pp_dev_json_content{margin:0;padding:6px 8px;background:#131210;border:1px solid #2d2a24;\
    color:#c6e377;white-space:pre-wrap;word-break:break-all;cursor:pointer;border-radius:3px;\
    font-size:10.5px;max-height:40vh;overflow:auto;}\
    .__pp_dev_json_content:hover{color:#fff;}\
    .__pp_dev_json_content.__pp_dev_copied{color:#181715;background:#ff6600;}\
    .__pp_dev_highlight{outline:2px solid #ff6600 !important;}\
";

