//! RFC-099 Phase 2 — server-side template stamper.
//!
//! Walks a compiled component's bindings + interpolations over its
//! **cleaned template HTML**, fills them from the component's state via
//! the host expression evaluator ([`pocopine_core::host_eval`]), and
//! serializes the result back to an HTML string — entirely host-side,
//! no wasm / no live DOM. The browser then *claims* this HTML during
//! hydration (Phase 2c).
//!
//! It mirrors the **string semantics** of the client's directive apply
//! functions (it does not call them — they mutate `web_sys` nodes):
//!
//! * `pp-text` → element text content (escaped on serialize),
//! * `pp-html` → element inner HTML (raw; only a string value),
//! * `pp-bind:<attr>` → set/remove an attribute (null/false remove;
//!   `class`/`style` object serialization; `data-*`/`aria-*` bool
//!   rendering),
//! * `pp-show` → merge `display:none` into the inline style when falsy,
//! * `{{interp}}` → replace the target text node with the rendered
//!   segments.
//!
//! Number coercion matches the client *per path*: text/interp use the
//! JS display formatter ([`pocopine_core::js_number`]); a plain `pp-bind`
//! attribute uses Rust `f64::to_string` (exactly what `bind.rs` does).
//!
//! **Phase-2 scope.** Structural controllers (`pp-if` / `pp-for` /
//! `pp-match`) resolve client-side and are left untouched here. Real-
//! component server↔client byte-equality is gated by the Phase-2c
//! differential render harness; this crate's unit test pins the stamp
//! semantics against hand-built fixtures.

use std::cell::RefCell;
use std::rc::Rc;

use html5ever::serialize::{SerializeOpts, TraversalScope};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::TreeBuilderOpts;
use html5ever::{
    local_name, namespace_url, ns, parse_fragment, serialize, Attribute, LocalName, ParseOpts,
    QualName,
};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom, SerializableHandle};
use pocopine_core::directives::for_plan::{BindingKind, StaticBinding, StaticInterp};
use pocopine_core::directives::interp::PlannedSegment;
use pocopine_core::{expr, host_eval, js_number};
use serde_json::Value;

/// Stamp one component's `bindings` + `interps` into its `cleaned_html`,
/// reading values from `state`. Returns the rendered HTML. If the
/// cleaned HTML has no element root, it is returned unchanged.
pub fn stamp(
    cleaned_html: &str,
    bindings: &[StaticBinding],
    interps: &[StaticInterp],
    state: &Value,
) -> String {
    // `markup5ever_rcdom::RcDom` clears its whole tree on `Drop` (it
    // detaches children iteratively to avoid deep-recursion stack
    // overflow), so a parsed node is only valid while its `RcDom` is
    // alive. Keep every dom we parse (the template + any `pp-html`
    // re-parses) alive until after serialization.
    let mut keep: Vec<RcDom> = Vec::new();
    let roots = parse_fragment_into(cleaned_html, local_name!("body"), &mut keep);
    let Some(root) = roots
        .into_iter()
        .find(|n| matches!(n.data, NodeData::Element { .. }))
    else {
        return cleaned_html.to_string();
    };
    for b in bindings {
        if let Some(node) = walk_element_path(&root, b.node_path) {
            apply_binding(&node, b, state, &mut keep);
        }
    }
    for it in interps {
        if let Some(parent) = walk_element_path(&root, it.node_path) {
            apply_interp(&parent, it, state);
        }
    }
    let out = serialize_node(&root);
    drop(keep); // tree must outlive the serialize above
    out
}

// ─── HTML parse / walk / serialize (html5ever RcDom) ───────────────

/// Parse an HTML fragment in the given context element, returning the
/// real top-level nodes (unwrapping html5ever's synthetic `<html>`
/// fragment wrapper — same recipe as `pocopine-template-parser`). The
/// owning `RcDom` is pushed into `keep` so the returned nodes stay
/// valid (see [`stamp`] for why).
fn parse_fragment_into(html: &str, ctx: LocalName, keep: &mut Vec<RcDom>) -> Vec<Handle> {
    let opts = ParseOpts {
        tree_builder: TreeBuilderOpts {
            drop_doctype: true,
            ..TreeBuilderOpts::default()
        },
        ..ParseOpts::default()
    };
    let context = QualName::new(None, ns!(html), ctx);
    let dom = parse_fragment(RcDom::default(), opts, context, Vec::new()).one(html.to_string());
    let roots = {
        let doc = dom.document.children.borrow();
        doc.iter()
            .find_map(|child| match &child.data {
                NodeData::Element { name, .. } if name.local == local_name!("html") => {
                    Some(child.children.borrow().clone())
                }
                _ => None,
            })
            .unwrap_or_else(|| doc.clone())
    };
    keep.push(dom);
    roots
}

/// Walk a node path of **element-child** indices (text/comment nodes
/// don't shift the index — matching the client's
/// `first_element_child`/`next_element_sibling` walk and the macro's
/// element-only `node_path`). An empty path is the root itself.
fn walk_element_path(root: &Handle, path: &[u16]) -> Option<Handle> {
    let mut cur = root.clone();
    for &idx in path {
        let next = cur
            .children
            .borrow()
            .iter()
            .filter(|c| matches!(c.data, NodeData::Element { .. }))
            .nth(idx as usize)
            .cloned();
        cur = next?;
    }
    Some(cur)
}

fn serialize_node(node: &Handle) -> String {
    let mut buf = Vec::new();
    let opts = SerializeOpts {
        traversal_scope: TraversalScope::IncludeNode,
        ..SerializeOpts::default()
    };
    serialize(&mut buf, &SerializableHandle::from(node.clone()), opts).expect("serialize RcDom");
    String::from_utf8(buf).expect("serialized HTML is UTF-8")
}

fn text_node(text: &str) -> Handle {
    Node::new(NodeData::Text {
        contents: RefCell::new(StrTendril::from(text)),
    })
}

// ─── binding application (mirrors directives::{text,html,bind,show}) ─

fn apply_binding(node: &Handle, b: &StaticBinding, state: &Value, keep: &mut Vec<RcDom>) {
    let v = eval_src(b.expr_src, state);
    match &b.kind {
        BindingKind::Text => set_text_content(node, &display_string(&v)),
        BindingKind::Html => set_inner_html(node, v.as_str(), keep),
        BindingKind::Show => {
            if is_falsy(&v) {
                merge_display_none(node);
            }
        }
        BindingKind::Bind { arg } => apply_bind(node, arg, &v),
        // `Class` is emitted only by RFC-054 row plans, never whole-
        // template plans (mirrors `install_static_binding`).
        BindingKind::Class => {}
    }
}

/// `textContent` — replace all children with a single text node.
fn set_text_content(node: &Handle, text: &str) {
    let t = text_node(text);
    t.parent.set(Some(Rc::downgrade(node)));
    *node.children.borrow_mut() = vec![t];
}

/// `innerHTML` — re-parse the (string) value in this element's context
/// and replace its children. A non-string value clears the children.
fn set_inner_html(node: &Handle, html: Option<&str>, keep: &mut Vec<RcDom>) {
    let ctx = match &node.data {
        NodeData::Element { name, .. } => name.local.clone(),
        _ => local_name!("div"),
    };
    let kids = match html {
        Some(h) => parse_fragment_into(h, ctx, keep),
        None => Vec::new(),
    };
    for k in &kids {
        k.parent.set(Some(Rc::downgrade(node)));
    }
    *node.children.borrow_mut() = kids;
}

fn apply_bind(node: &Handle, arg: &str, v: &Value) {
    // Falsy removal: undefined/null/false → drop the attribute (the
    // client's `apply_memoised` does this before serialization).
    if matches!(v, Value::Null | Value::Bool(false)) {
        remove_attribute(node, arg);
        return;
    }
    let serialised = match arg {
        "class" => serialise_class(v),
        "style" => serialise_style(v),
        _ => Some(serialise_plain(arg, v)),
    };
    if let Some(s) = serialised {
        set_attribute(node, arg, &s);
    }
}

fn serialise_class(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        // Object: space-join truthy keys (key order follows the map).
        Value::Object(map) => Some(
            map.iter()
                .filter(|(_, val)| !is_falsy(val))
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

fn serialise_style(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => Some(
            map.iter()
                .map(|(k, val)| format!("{k}:{};", val.as_str().unwrap_or_default()))
                .collect::<String>(),
        ),
        _ => None,
    }
}

fn serialise_plain(attr: &str, v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        // Plain attrs use Rust `f64::to_string` (exactly `bind.rs`),
        // NOT the JS display formatter used for text/interp.
        Value::Number(n) => n.as_f64().map(|f| f.to_string()).unwrap_or_default(),
        // `false` is removed upstream; only `true` reaches here.
        Value::Bool(_) => {
            if attr.starts_with("data-") || attr.starts_with("aria-") {
                "true".to_string()
            } else {
                String::new() // present-with-empty (classic attr `true`)
            }
        }
        // Arrays/objects → JSON (mirrors `JSON.stringify`).
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `pp-show` falsy → merge `display:none` into the inline `style` attr.
fn merge_display_none(node: &Handle) {
    let mut style = get_attribute(node, "style").unwrap_or_default();
    let trimmed = style.trim_end();
    if !trimmed.is_empty() && !trimmed.ends_with(';') {
        style.push(';');
    }
    style.push_str("display:none;");
    set_attribute(node, "style", &style);
}

// ─── interpolation ─────────────────────────────────────────────────

fn apply_interp(parent: &Handle, it: &StaticInterp, state: &Value) {
    let rendered = render_segments(it.segments, state);
    // Replace the `text_index`-th TEXT-node child (text nodes only
    // count, matching `interp::resolve_text_target`).
    {
        let children = parent.children.borrow();
        let mut seen = 0u16;
        for c in children.iter() {
            if let NodeData::Text { ref contents } = c.data {
                if seen == it.text_index {
                    *contents.borrow_mut() = StrTendril::from(rendered.as_str());
                    return;
                }
                seen += 1;
            }
        }
    }
    // Target text node absent (cleaning may have collapsed an empty
    // one). Best-effort append — real-component placement is gated by
    // the Phase-2c differential harness.
    let t = text_node(&rendered);
    t.parent.set(Some(Rc::downgrade(parent)));
    parent.children.borrow_mut().push(t);
}

fn render_segments(segments: &[PlannedSegment], state: &Value) -> String {
    let mut out = String::new();
    for seg in segments {
        match seg {
            PlannedSegment::Static(t) => out.push_str(t),
            PlannedSegment::Dynamic(src) => out.push_str(&display_string(&eval_src(src, state))),
        }
    }
    out
}

// ─── attribute helpers ─────────────────────────────────────────────

fn attr_qual(name: &str) -> QualName {
    QualName::new(None, ns!(), LocalName::from(name))
}

fn set_attribute(node: &Handle, name: &str, value: &str) {
    if let NodeData::Element { ref attrs, .. } = node.data {
        let qn = attr_qual(name);
        let mut attrs = attrs.borrow_mut();
        if let Some(existing) = attrs.iter_mut().find(|a| a.name == qn) {
            existing.value = StrTendril::from(value);
        } else {
            attrs.push(Attribute {
                name: qn,
                value: StrTendril::from(value),
            });
        }
    }
}

fn remove_attribute(node: &Handle, name: &str) {
    if let NodeData::Element { ref attrs, .. } = node.data {
        let qn = attr_qual(name);
        attrs.borrow_mut().retain(|a| a.name != qn);
    }
}

fn get_attribute(node: &Handle, name: &str) -> Option<String> {
    if let NodeData::Element { ref attrs, .. } = node.data {
        let qn = attr_qual(name);
        return attrs
            .borrow()
            .iter()
            .find(|a| a.name == qn)
            .map(|a| a.value.to_string());
    }
    None
}

// ─── value → string coercion + eval ────────────────────────────────

/// Display coercion for text / interp — mirrors `directives` `js_to_string`:
/// null → "", string → itself, number → JS formatter, bool →
/// "true"/"false", array/object → JSON.
fn display_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.as_f64().map(js_number::to_js_string).unwrap_or_default(),
        Value::Bool(b) => b.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// JS truthiness (mirrors `host_eval`'s, which mirrors `JsValue::is_falsy`).
fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f == 0.0),
        Value::String(s) => s.is_empty(),
        _ => false,
    }
}

fn eval_src(src: &str, state: &Value) -> Value {
    match expr::parse(src) {
        Ok(ast) => host_eval::eval(&ast, state),
        Err(_) => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::stamp;
    use pocopine_core::directives::for_plan::{BindingKind, StaticBinding, StaticInterp};
    use pocopine_core::directives::interp::PlannedSegment;
    use serde_json::json;

    #[test]
    fn stamps_text_bind_show_and_interp() {
        // root <div>; element children: [0]=<span>, [1]=<p>, [2]=<b>.
        let html = r#"<div data-pp-scope-id="demo"><span class="t"></span><p style="color:red"></p><b>placeholder</b></div>"#;
        let bindings: &[StaticBinding] = &[
            StaticBinding {
                node_path: &[0],
                kind: BindingKind::Text,
                expr_src: "title",
                compiled: None,
            },
            StaticBinding {
                node_path: &[1],
                kind: BindingKind::Bind { arg: "data-n" },
                expr_src: "count",
                compiled: None,
            },
            StaticBinding {
                node_path: &[1],
                kind: BindingKind::Show,
                expr_src: "visible",
                compiled: None,
            },
        ];
        let interps: &[StaticInterp] = &[StaticInterp {
            node_path: &[2],
            text_index: 0,
            segments: &[
                PlannedSegment::Static("hello "),
                PlannedSegment::Dynamic("name"),
            ],
        }];
        let state = json!({ "title": "Hi & bye", "count": 5, "visible": false, "name": "world" });
        let out = stamp(html, bindings, interps, &state);

        // pp-text: textContent escaped on serialize
        assert!(
            out.contains("<span class=\"t\">Hi &amp; bye</span>"),
            "text: {out}"
        );
        // pp-bind: data-n attribute (Rust number formatting)
        assert!(out.contains("data-n=\"5\""), "bind: {out}");
        // pp-show falsy: display:none merged into existing style
        assert!(out.contains("display:none"), "show: {out}");
        // interp: static + dynamic, replacing the placeholder text node
        assert!(out.contains("<b>hello world</b>"), "interp: {out}");
    }

    #[test]
    fn bind_removes_attr_on_falsy_and_serialises_class_object() {
        let html = r#"<div><a href="/old" class="base"></a></div>"#;
        let bindings: &[StaticBinding] = &[
            // null → remove `href`
            StaticBinding {
                node_path: &[0],
                kind: BindingKind::Bind { arg: "href" },
                expr_src: "link",
                compiled: None,
            },
            // class object → space-joined truthy keys
            StaticBinding {
                node_path: &[0],
                kind: BindingKind::Bind { arg: "class" },
                expr_src: "classes",
                compiled: None,
            },
        ];
        let state =
            json!({ "link": null, "classes": { "active": true, "muted": false, "lg": true } });
        let out = stamp(html, bindings, &[], &state);
        assert!(!out.contains("href"), "href should be removed: {out}");
        // BTreeMap key order (active, lg) — both truthy, `muted` dropped.
        assert!(out.contains("class=\"active lg\""), "class object: {out}");
    }
}
