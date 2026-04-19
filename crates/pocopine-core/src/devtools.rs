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
use web_sys::{window, Document, Element, HtmlElement, KeyboardEvent};

use crate::refs;
use crate::scope::Scope;

const STYLE_ID: &str = "__pp_devtools_style";
const ROOT_ID: &str = "__pp_devtools_root";
const HIGHLIGHT_CLASS: &str = "__pp_dev_highlight";

type RenderClosure = Closure<dyn FnMut()>;
type KeyClosure = Closure<dyn FnMut(KeyboardEvent)>;

thread_local! {
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
    static VISIBLE: Cell<bool> = const { Cell::new(true) };
    static INTERVAL_ID: Cell<Option<i32>> = const { Cell::new(None) };
    static RENDER_CB: RefCell<Option<RenderClosure>> = RefCell::new(None);
    static KEY_CB: RefCell<Option<KeyClosure>> = RefCell::new(None);
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
    }

    let scopes = Scope::all();
    let mut html = String::from(
        r#"<header class="__pp_dev_header">
             <span>pocopine devtools</span>
             <button class="__pp_dev_close" onclick="this.parentElement.parentElement.style.display='none'">×</button>
           </header>
           <div class="__pp_dev_meta">"#,
    );
    html.push_str(&format!("{} scopes", scopes.len()));
    html.push_str("</div><div class=\"__pp_dev_scopes\">");

    for scope in scopes.iter() {
        let state = scope.state.borrow();
        let tag = escape(state.type_name());
        let keys = state.keys();
        html.push_str(&format!(
            "<section class=\"__pp_dev_scope\" data-scope-id=\"{id}\">\
               <div class=\"__pp_dev_scope_hd\">\
                 <span class=\"__pp_dev_tag\">&lt;{tag}&gt;</span>\
                 <span class=\"__pp_dev_id\">#{id}</span>\
               </div>\
               <dl class=\"__pp_dev_kv\">",
            id = scope.id.0,
            tag = tag,
        ));
        if keys.is_empty() {
            html.push_str("<div class=\"__pp_dev_empty\">no declared fields</div>");
        } else {
            for key in keys {
                let v = state.get(key);
                let value_str = stringify(&v);
                html.push_str(&format!(
                    "<div class=\"__pp_dev_row\">\
                       <dt>{k}</dt>\
                       <dd>{v}</dd>\
                     </div>",
                    k = escape(key),
                    v = escape(&value_str),
                ));
            }
        }
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
        html.push_str("</dl></section>");
    }
    html.push_str("</div>");
    root.set_inner_html(&html);
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
    el
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
    #__pp_devtools_root{position:fixed;top:12px;right:12px;width:320px;max-height:70vh;\
    overflow:auto;z-index:2147483647;font-family:ui-monospace,Menlo,Consolas,monospace;\
    font-size:11px;line-height:1.45;color:#e6e1d8;background:#181715;border:1px solid #2d2a24;\
    border-radius:8px;box-shadow:0 10px 30px rgba(0,0,0,0.5);}\
    #__pp_devtools_root *{box-sizing:border-box;}\
    .__pp_dev_header{display:flex;align-items:center;justify-content:space-between;\
    padding:8px 12px;background:#252220;border-bottom:1px solid #2d2a24;position:sticky;top:0;\
    color:#ff6600;letter-spacing:0.06em;text-transform:uppercase;font-size:10px;}\
    .__pp_dev_close{background:none;border:none;color:#e6e1d8;font-size:16px;line-height:1;\
    cursor:pointer;padding:0 4px;}\
    .__pp_dev_meta{padding:6px 12px;color:#828282;border-bottom:1px solid #2d2a24;}\
    .__pp_dev_scopes{padding:4px 8px;}\
    .__pp_dev_scope{padding:6px 4px;border-bottom:1px dashed #2d2a24;}\
    .__pp_dev_scope:last-child{border-bottom:none;}\
    .__pp_dev_scope_hd{display:flex;justify-content:space-between;align-items:baseline;\
    padding:2px 4px;}\
    .__pp_dev_tag{color:#ff6600;}\
    .__pp_dev_id{color:#828282;font-size:10px;}\
    .__pp_dev_kv{margin:2px 0 0;padding:0 4px;}\
    .__pp_dev_row{display:flex;gap:8px;padding:1px 0;}\
    .__pp_dev_row dt{color:#e6e1d8;flex:0 0 auto;min-width:7em;opacity:0.75;}\
    .__pp_dev_row dd{color:#c6e377;margin:0;word-break:break-all;}\
    .__pp_dev_empty{color:#828282;font-style:italic;padding:2px 0;}\
    .__pp_dev_refs{margin-top:4px;color:#828282;font-size:10px;padding:2px 4px;}\
    .__pp_dev_refs code{color:#c6e377;}\
    .__pp_dev_highlight{outline:2px solid #ff6600 !important;}\
";

// Silence unused-const warning.
#[allow(dead_code)]
const _H: &str = HIGHLIGHT_CLASS;
