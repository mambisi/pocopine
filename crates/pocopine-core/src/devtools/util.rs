//! Pure helpers shared by every devtools panel: HTML escaping,
//! JsValue → display-string formatting, UTF-8-safe middle truncation,
//! and the collapsible JSON tree builder.
//!
//! These live here (not in the panel files) so they stay testable
//! without a DOM. The pre-split monolithic devtools inlined them into
//! the panel body — the refactor moves them up-module so PR C's
//! timeline panel, PR D's signal graph, etc. can share the same
//! `stringify` / `raw_value` / `escape` + json-view implementations
//! without duplicating the edge cases.

use js_sys::{Array, Object};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;

/// Max characters a value cell shows before the middle is replaced
/// with `…`. Chosen to roughly fit the 540px panel at the monospace
/// size. Exposed pub(super) so panels can reuse the cap for their
/// own cell budgets.
pub(super) const MAX_VALUE_CHARS: usize = 80;

/// HTML-escape `s` for safe insertion into an attribute or text node.
/// The current panel only ever builds strings that get handed to
/// `set_inner_html`, so escaping is non-negotiable.
pub(crate) fn escape(s: &str) -> String {
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

/// Clamp `s` to [`MAX_VALUE_CHARS`], replacing the interior with `…`
/// so both the start and end stay readable. Walks by `chars()`, not
/// bytes — safe for multibyte UTF-8 input.
pub(crate) fn truncate(s: &str) -> String {
    let n = s.chars().count();
    if n <= MAX_VALUE_CHARS {
        return s.to_string();
    }
    let budget = MAX_VALUE_CHARS.saturating_sub(1);
    let head_len = budget.div_ceil(2);
    let tail_len = budget - head_len;
    let head: String = s.chars().take(head_len).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(tail_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// Best-effort string view of a `JsValue` for display in the panel.
/// Arrays collapse to a length summary; long strings / objects get
/// truncated in the middle so both ends stay visible.
pub(crate) fn stringify(v: &JsValue) -> String {
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
    if Array::is_array(v) {
        let len = Array::from(v).length();
        return format!("[{len} items]");
    }
    let raw = js_sys::JSON::stringify(v)
        .ok()
        .and_then(|s| s.as_string())
        .unwrap_or_else(|| "?".into());
    truncate(&raw)
}

/// Clipboard-friendly form of a `JsValue`. Strings come through
/// unquoted so paste is direct; everything else is its canonical JS /
/// JSON representation, not truncated.
pub(crate) fn raw_value(v: &JsValue) -> String {
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

/// Render a `JsValue` as a collapsible HTML tree. Uses native
/// `<details>` / `<summary>` so no per-node JavaScript is needed —
/// the browser owns the expand/collapse state. Primitives stay
/// clickable via `data-copy` so any leaf can be copied.
///
/// Future: swap each primitive `<span>` for a `<span contenteditable>`
/// that writes back through the scope's proxy. The classification
/// branches here already commit the tree to a shape that supports it.
pub(crate) fn build_json_view(v: &JsValue, is_root: bool) -> String {
    if v.is_undefined() {
        return r#"<span class="__pp_jv_null" data-copy="undefined">undefined</span>"#.into();
    }
    if v.is_null() {
        return r#"<span class="__pp_jv_null" data-copy="null">null</span>"#.into();
    }
    if let Some(s) = v.as_string() {
        return format!(
            "<span class=\"__pp_jv_string\" data-copy=\"{}\">\"{}\"</span>",
            escape(&s),
            escape(&s)
        );
    }
    if let Some(n) = v.as_f64() {
        let n_str = n.to_string();
        return format!("<span class=\"__pp_jv_number\" data-copy=\"{n_str}\">{n_str}</span>");
    }
    if let Some(b) = v.as_bool() {
        let b_str = b.to_string();
        return format!("<span class=\"__pp_jv_bool\" data-copy=\"{b_str}\">{b_str}</span>");
    }
    if Array::is_array(v) {
        let arr = Array::from(v);
        let len = arr.length();
        if len == 0 {
            return r#"<span class="__pp_jv_empty">[ ]</span>"#.into();
        }
        let open = if is_root { " open" } else { "" };
        let mut rows = String::new();
        for i in 0..len {
            let item = arr.get(i);
            rows.push_str(&format!(
                "<div class=\"__pp_jv_row\">\
                   <span class=\"__pp_jv_key\">{i}</span>\
                   <span class=\"__pp_jv_colon\">:</span> {val}\
                 </div>",
                val = build_json_view(&item, false)
            ));
        }
        return format!(
            "<details class=\"__pp_jv_group\"{open}>\
               <summary class=\"__pp_jv_summary\">\
                 <span class=\"__pp_jv_bracket\">[</span>\
                 <span class=\"__pp_jv_meta\">{len} items</span>\
                 <span class=\"__pp_jv_bracket\">]</span>\
               </summary>\
               <div class=\"__pp_jv_body\">{rows}</div>\
             </details>"
        );
    }
    if v.is_object() {
        let obj: Object = v.clone().unchecked_into();
        let keys = Object::keys(&obj);
        let n = keys.length();
        if n == 0 {
            return r#"<span class="__pp_jv_empty">{ }</span>"#.into();
        }
        let open = if is_root { " open" } else { "" };
        let mut rows = String::new();
        for i in 0..n {
            let key = keys.get(i);
            let key_str = key.as_string().unwrap_or_default();
            let value = js_sys::Reflect::get(&obj, &key).unwrap_or(JsValue::UNDEFINED);
            rows.push_str(&format!(
                "<div class=\"__pp_jv_row\">\
                   <span class=\"__pp_jv_key\">{k}</span>\
                   <span class=\"__pp_jv_colon\">:</span> {val}\
                 </div>",
                k = escape(&key_str),
                val = build_json_view(&value, false)
            ));
        }
        return format!(
            "<details class=\"__pp_jv_group\"{open}>\
               <summary class=\"__pp_jv_summary\">\
                 <span class=\"__pp_jv_bracket\">{{</span>\
                 <span class=\"__pp_jv_meta\">{n} keys</span>\
                 <span class=\"__pp_jv_bracket\">}}</span>\
               </summary>\
               <div class=\"__pp_jv_body\">{rows}</div>\
             </details>"
        );
    }
    r#"<span class="__pp_jv_null">?</span>"#.into()
}

// ── tests ─────────────────────────────────────────────────────────
//
// Only the purely-string helpers have coverage — `stringify` /
// `raw_value` / `build_json_view` branch on `JsValue` and need wasm
// to exercise. Those get coverage via wasm-pack test later.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_turns_reserved_html_chars_into_entities() {
        assert_eq!(escape("<div>"), "&lt;div&gt;");
        assert_eq!(escape("a & b"), "a &amp; b");
        assert_eq!(escape("\"q\""), "&quot;q&quot;");
        assert_eq!(escape("'apos'"), "&#39;apos&#39;");
        assert_eq!(escape("plain text"), "plain text");
    }

    #[test]
    fn truncate_passes_through_short_input() {
        let short = "abcd";
        assert_eq!(truncate(short), short);
    }

    #[test]
    fn truncate_clamps_and_inserts_middle_ellipsis_on_long_input() {
        let long: String = "a".repeat(200);
        let out = truncate(&long);
        assert!(out.contains('…'));
        assert!(out.chars().count() <= MAX_VALUE_CHARS);
    }

    #[test]
    fn truncate_keeps_head_and_tail_visible() {
        let s =
            "start_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx_end";
        let out = truncate(s);
        // both ends should survive
        assert!(out.starts_with("start"));
        assert!(out.ends_with("end"));
        assert!(out.contains('…'));
    }

    #[test]
    fn truncate_is_utf8_safe() {
        // multibyte chars shouldn't panic or split mid-codepoint
        let long: String = "日".repeat(100);
        let out = truncate(&long);
        assert!(out.chars().count() <= MAX_VALUE_CHARS);
        // contains at least one ellipsis
        assert!(out.contains('…'));
    }
}
