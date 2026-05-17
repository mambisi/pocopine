//! Render a model doc to an HTML string.
//!
//! Each emitted block element carries a `data-pos` attribute set to the
//! model position at its outer boundary (the position right before the
//! element in its parent's content). The selection bridge uses these
//! markers to translate between DOM ranges (`web_sys::Range`) and model
//! positions: given a DOM node, walk up to find the nearest element
//! with `data-pos`, then add the offset of the user's caret inside it.
//! The inverse direction (model position → DOM `(node, offset)`) walks
//! the DOM forward summing sizes until the target position is reached.
//!
//! Inline mark wrappers (em / strong / code / a) do NOT get `data-pos`;
//! they're decorations inside a textblock and the bridge handles them
//! by summing the text content of preceding siblings.
//!
//! Schema assumptions match `schema_basic`:
//! - `doc` → wraps `block+` children with no surrounding tag.
//! - `paragraph` → `<p>`. `heading` → `<h{level}>`.
//! - `blockquote` → `<blockquote>`.
//! - `bullet_list` / `ordered_list` → `<ul>` / `<ol>`.
//! - `list_item` → `<li>`.
//! - `code_block` → `<pre><code>` (one wrapper carries `data-pos`).
//! - `horizontal_rule` → `<hr/>`, `image` → `<img/>`, `hard_break` → `<br/>`.
//! - `text` → escaped text content, marks wrap outside-in.
//!
//! Unknown node types fall back to `<span data-type="{name}">…</span>`
//! around their children so apps spot missing renderer cases without
//! losing data.

use crate::model::{Mark, Node};

pub mod node_views {
    //! Per-node-type renderer overrides.
    //!
    //! Apps call [`register`] at boot to map a model node type (e.g.
    //! `"task_item"`) to a custom-element tag (e.g. `"pine-task-item"`).
    //! The renderer then emits the custom element instead of pine's
    //! default tag, surfacing every model attribute as a `data-<attr>`
    //! attribute so the component author can read them from the DOM
    //! and react to changes.
    //!
    //! Node-view hosts are still model-owned DOM. The component may own
    //! wrapper chrome such as checkboxes or handles, but pine owns the
    //! host element, its `data-pos`, its reflected `data-*` model attrs,
    //! and the rendered model children. If the component wraps children
    //! in a content element, register that element's selector so the
    //! browser view can reconcile model children without recreating the
    //! component chrome. Attr-only transactions update host `data-*`
    //! attrs in place so components keep their DOM identity and event
    //! listeners.
    //!
    //! This is pocopine's equivalent of Tiptap's `addNodeView` — a
    //! contract that lets a component own the wrapper chrome (drag
    //! handles, checkboxes, embed controls) while pine keeps owning
    //! the inline content it renders inside.
    use std::collections::BTreeMap;
    use std::sync::{OnceLock, RwLock};

    /// What to emit when a registered node type is rendered.
    #[derive(Clone, Debug)]
    pub struct NodeViewSpec {
        /// Custom-element tag the renderer emits in place of the
        /// default. Must be valid HTML (lowercase, contains a hyphen).
        pub tag: String,
        /// Optional selector, relative to the custom-element host, for
        /// the element that contains model-owned children after the
        /// component mounts. Components with wrapper chrome should set
        /// this; components that leave children directly under the host
        /// can use [`register`].
        pub content_selector: Option<String>,
    }

    fn registry() -> &'static RwLock<BTreeMap<String, NodeViewSpec>> {
        static REGISTRY: OnceLock<RwLock<BTreeMap<String, NodeViewSpec>>> = OnceLock::new();
        REGISTRY.get_or_init(|| RwLock::new(BTreeMap::new()))
    }

    /// Register `tag` as the custom-element tag the renderer should
    /// emit for nodes of type `node_type`. Calling twice with the same
    /// type replaces the prior registration.
    pub fn register(node_type: impl Into<String>, tag: impl Into<String>) {
        let mut map = registry().write().expect("node_view registry poisoned");
        map.insert(
            node_type.into(),
            NodeViewSpec {
                tag: tag.into(),
                content_selector: None,
            },
        );
    }

    /// Register a custom-element node view whose model-owned children
    /// live under `content_selector` after the component mounts.
    pub fn register_with_content(
        node_type: impl Into<String>,
        tag: impl Into<String>,
        content_selector: impl Into<String>,
    ) {
        let mut map = registry().write().expect("node_view registry poisoned");
        map.insert(
            node_type.into(),
            NodeViewSpec {
                tag: tag.into(),
                content_selector: Some(content_selector.into()),
            },
        );
    }

    /// Look up a registered node-view spec, if any.
    pub fn lookup(node_type: &str) -> Option<NodeViewSpec> {
        registry()
            .read()
            .expect("node_view registry poisoned")
            .get(node_type)
            .cloned()
    }

    /// Return the registered custom-element tags. The browser view
    /// uses this after `innerHTML` patches so dynamically-rendered
    /// node-view hosts are mounted like app-authored components.
    pub fn registered_tags() -> Vec<String> {
        registry()
            .read()
            .expect("node_view registry poisoned")
            .values()
            .map(|spec| spec.tag.clone())
            .collect()
    }

    /// Remove the registration for `node_type`. Used in tests so global
    /// state doesn't bleed between cases.
    pub fn unregister(node_type: &str) {
        registry()
            .write()
            .expect("node_view registry poisoned")
            .remove(node_type);
    }
}

use node_views::NodeViewSpec;

/// Emit a node as the custom element registered via
/// [`node_views::register`]. Every model attribute is mirrored onto the
/// element as `data-<attr>=<json>` so the component can read it from
/// the DOM. Children are rendered inline as content — the component
/// is expected to lay them out (typically by just letting them render
/// where they sit in its template / Shadow DOM).
fn render_node_view(node: &Node, spec: &NodeViewSpec, outer_pos: usize, out: &mut String) {
    out.push('<');
    out.push_str(&spec.tag);
    out.push_str(" data-pos=\"");
    out.push_str(&outer_pos.to_string());
    out.push('"');
    for (key, value) in node.attrs() {
        out.push_str(" data-");
        out.push_str(&escape_attr(key));
        out.push_str("=\"");
        out.push_str(&escape_attr(&attr_value_to_string(value)));
        out.push('"');
    }
    out.push('>');
    if node.content().size() == 0 && !node.is_leaf() {
        out.push_str("<br/>");
    } else {
        let mut child_pos = outer_pos + 1;
        for child in node.content().iter() {
            render_node(child, child_pos, out);
            child_pos += child.node_size();
        }
    }
    out.push_str("</");
    out.push_str(&spec.tag);
    out.push('>');
}

pub(crate) fn attr_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Walk `doc` and produce the matching HTML. The doc node itself isn't
/// wrapped; its children are emitted directly so the caller can set the
/// result on a surface's `innerHTML`. The surface element implicitly
/// represents the doc and "owns" position 0.
pub fn render_doc_to_html(doc: &Node) -> String {
    let mut out = String::new();
    // The doc's content starts at position 0. As we walk its children
    // we tag each block with its outer-position so the selection bridge
    // can map DOM cursors back to model positions.
    let mut pos = 0usize;
    for child in doc.content().iter() {
        render_node(child, pos, &mut out);
        pos += child.node_size();
    }
    out
}

/// Render the CHILDREN of `node` (no wrapper for `node` itself) at the
/// given content-start position. Used by reconcilers that have located
/// the DOM element for `node` and need to refresh just its inner
/// contents.
pub fn render_children_to_html(node: &Node, content_start: usize) -> String {
    let mut out = String::new();
    if node.content().size() == 0 && !node.is_text() && !node.is_leaf() {
        // Block placeholder so contentEditable renders a clickable line.
        out.push_str("<br/>");
        return out;
    }
    let mut pos = content_start;
    for child in node.content().iter() {
        render_node(child, pos, &mut out);
        pos += child.node_size();
    }
    out
}

/// Render a single node (with its wrapper) at the given outer position.
/// Used by reconcilers that want to swap one element out of a parent
/// without re-rendering the parent's other children.
pub fn render_one_node_to_html(node: &Node, outer_pos: usize) -> String {
    let mut out = String::new();
    render_node(node, outer_pos, &mut out);
    out
}

/// `outer_pos` is the model position right BEFORE `node` in its
/// parent's content. The emitted element's `data-pos` matches that;
/// callers (the selection bridge) treat content-start as `outer_pos + 1`
/// for non-leaf nodes, or treat the leaf as a single position.
fn render_node(node: &Node, outer_pos: usize, out: &mut String) {
    if node.is_text() {
        let text = node.text().unwrap_or("");
        render_text(text, node.marks(), out);
        return;
    }

    // If a node view is registered for this node type, render it as
    // the custom element instead of the default tag. The component
    // is responsible for any wrapper chrome (drag handle, checkbox,
    // action buttons); pine still owns the inline content rendered
    // inside.
    if let Some(spec) = node_views::lookup(node.type_name()) {
        render_node_view(node, &spec, outer_pos, out);
        return;
    }

    match node.type_name() {
        "paragraph" => render_block(node, "p", outer_pos, out),
        "blockquote" => render_block(node, "blockquote", outer_pos, out),
        "bullet_list" => render_block(node, "ul", outer_pos, out),
        "ordered_list" => render_ordered_list(node, outer_pos, out),
        "list_item" => render_block(node, "li", outer_pos, out),
        "task_list" => render_task_list(node, outer_pos, out),
        "task_item" => render_task_item(node, outer_pos, out),
        "heading" => {
            let level = node
                .attrs()
                .get("level")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .clamp(1, 6);
            let tag = format!("h{level}");
            render_block(node, &tag, outer_pos, out);
        }
        "code_block" => {
            // `data-pos` on the outer `<pre>`. The `<code>` wrapper has no
            // `data-pos` — it's just there for default monospace styling.
            out.push_str("<pre data-pos=\"");
            out.push_str(&outer_pos.to_string());
            out.push_str("\"><code>");
            for child in node.content().iter() {
                render_node(child, outer_pos + 1, out);
            }
            out.push_str("</code></pre>");
        }
        "horizontal_rule" => {
            out.push_str("<hr data-pos=\"");
            out.push_str(&outer_pos.to_string());
            out.push_str("\"/>");
        }
        "hard_break" => {
            out.push_str("<br data-pos=\"");
            out.push_str(&outer_pos.to_string());
            out.push_str("\"/>");
        }
        "image" => {
            let src = node
                .attrs()
                .get("src")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let alt = node
                .attrs()
                .get("alt")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let title = node.attrs().get("title").and_then(|v| v.as_str());
            out.push_str("<img data-pos=\"");
            out.push_str(&outer_pos.to_string());
            out.push_str("\" src=\"");
            out.push_str(&escape_attr(src));
            out.push_str("\" alt=\"");
            out.push_str(&escape_attr(alt));
            out.push('"');
            if let Some(title) = title {
                out.push_str(" title=\"");
                out.push_str(&escape_attr(title));
                out.push('"');
            }
            out.push_str("/>");
        }
        other => {
            out.push_str("<span data-pos=\"");
            out.push_str(&outer_pos.to_string());
            out.push_str("\" data-type=\"");
            out.push_str(&escape_attr(other));
            out.push_str("\">");
            let mut child_pos = outer_pos + 1;
            for child in node.content().iter() {
                render_node(child, child_pos, out);
                child_pos += child.node_size();
            }
            out.push_str("</span>");
        }
    }
}

fn render_ordered_list(node: &Node, outer_pos: usize, out: &mut String) {
    out.push_str("<ol data-pos=\"");
    out.push_str(&outer_pos.to_string());
    out.push('"');
    if let Some(order) = node.attrs().get("order").and_then(|v| v.as_i64()) {
        if order != 1 {
            out.push_str(" start=\"");
            out.push_str(&order.to_string());
            out.push('"');
        }
    }
    out.push('>');
    if node.content().size() == 0 {
        out.push_str("<br/>");
    } else {
        let mut child_pos = outer_pos + 1;
        for child in node.content().iter() {
            render_node(child, child_pos, out);
            child_pos += child.node_size();
        }
    }
    out.push_str("</ol>");
}

/// `task_list` is just a `<ul>` with a marker class. CSS hides the
/// default bullet and styles items with a leading checkbox.
fn render_task_list(node: &Node, outer_pos: usize, out: &mut String) {
    out.push_str("<ul class=\"task-list\" data-pos=\"");
    out.push_str(&outer_pos.to_string());
    out.push_str("\">");
    if node.content().size() == 0 {
        out.push_str("<br/>");
    } else {
        let mut child_pos = outer_pos + 1;
        for child in node.content().iter() {
            render_node(child, child_pos, out);
            child_pos += child.node_size();
        }
    }
    out.push_str("</ul>");
}

/// `task_item` is a `<li class="task-item">` with a `data-checked`
/// attribute mirroring the model's `checked` attr. The leading checkbox
/// is drawn purely via CSS `::before` so no DOM nodes outside the model
/// content live inside the item — the selection bridge keeps working
/// unchanged.
fn render_task_item(node: &Node, outer_pos: usize, out: &mut String) {
    let checked = node
        .attrs()
        .get("checked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    out.push_str("<li class=\"task-item\" data-pos=\"");
    out.push_str(&outer_pos.to_string());
    out.push_str("\" data-checked=\"");
    out.push_str(if checked { "true" } else { "false" });
    out.push_str("\">");
    if node.content().size() == 0 {
        out.push_str("<br/>");
    } else {
        let mut child_pos = outer_pos + 1;
        for child in node.content().iter() {
            render_node(child, child_pos, out);
            child_pos += child.node_size();
        }
    }
    out.push_str("</li>");
}

fn render_block(node: &Node, tag: &str, outer_pos: usize, out: &mut String) {
    out.push('<');
    out.push_str(tag);
    out.push_str(" data-pos=\"");
    out.push_str(&outer_pos.to_string());
    out.push_str("\">");
    if node.content().size() == 0 {
        // Empty blocks need a placeholder child so contentEditable
        // renders a clickable line. `<br/>` doesn't carry a `data-pos`
        // — it's purely visual; the bridge treats it as if the cursor
        // is at the parent's content start (position `outer_pos + 1`).
        out.push_str("<br/>");
    } else {
        let mut child_pos = outer_pos + 1;
        for child in node.content().iter() {
            render_node(child, child_pos, out);
            child_pos += child.node_size();
        }
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn render_text(text: &str, marks: &[Mark], out: &mut String) {
    for mark in marks {
        match mark.type_name() {
            "em" => out.push_str("<em>"),
            "strong" => out.push_str("<strong>"),
            "code" => out.push_str("<code>"),
            "link" => {
                let href = mark
                    .attrs()
                    .get("href")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let title = mark.attrs().get("title").and_then(|v| v.as_str());
                out.push_str("<a href=\"");
                out.push_str(&escape_attr(href));
                out.push('"');
                if let Some(title) = title {
                    out.push_str(" title=\"");
                    out.push_str(&escape_attr(title));
                    out.push('"');
                }
                out.push('>');
            }
            other => {
                out.push_str("<span data-mark=\"");
                out.push_str(&escape_attr(other));
                out.push_str("\">");
            }
        }
    }
    out.push_str(&escape_text(text));
    for mark in marks.iter().rev() {
        let tag = match mark.type_name() {
            "em" => "em",
            "strong" => "strong",
            "code" => "code",
            "link" => "a",
            _ => "span",
        };
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    }
}

fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(ch),
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Attrs, Fragment};
    use crate::schema_basic;
    use serde_json::json;

    #[test]
    fn paragraphs_carry_their_outer_pos_in_data_pos() {
        let p1 =
            schema_basic::paragraph(vec![schema_basic::text("foo", Vec::new()).unwrap()]).unwrap();
        let p2 =
            schema_basic::paragraph(vec![schema_basic::text("bar", Vec::new()).unwrap()]).unwrap();
        let doc = schema_basic::doc(vec![p1, p2]).unwrap();
        // p1.node_size = 1 + 3 + 1 = 5 → second paragraph at outer pos 5.
        assert_eq!(
            render_doc_to_html(&doc),
            r#"<p data-pos="0">foo</p><p data-pos="5">bar</p>"#,
        );
    }

    #[test]
    fn renders_heading_with_level_and_pos() {
        let h = schema_basic::heading(2, vec![schema_basic::text("Title", Vec::new()).unwrap()])
            .unwrap();
        let doc = schema_basic::doc(vec![h]).unwrap();
        assert_eq!(render_doc_to_html(&doc), r#"<h2 data-pos="0">Title</h2>"#,);
    }

    #[test]
    fn wraps_marked_text_outside_in() {
        let em = schema_basic::em().unwrap();
        let strong = schema_basic::strong().unwrap();
        let marked = schema_basic::text("bold-em", vec![em.clone(), strong.clone()]).unwrap();
        let p = schema_basic::paragraph(vec![marked]).unwrap();
        let doc = schema_basic::doc(vec![p]).unwrap();
        assert_eq!(
            render_doc_to_html(&doc),
            r#"<p data-pos="0"><em><strong>bold-em</strong></em></p>"#,
        );
    }

    #[test]
    fn escapes_html_in_text() {
        let p = schema_basic::paragraph(vec![
            schema_basic::text("a < b & c > d", Vec::new()).unwrap()
        ])
        .unwrap();
        let doc = schema_basic::doc(vec![p]).unwrap();
        assert_eq!(
            render_doc_to_html(&doc),
            r#"<p data-pos="0">a &lt; b &amp; c &gt; d</p>"#,
        );
    }

    #[test]
    fn empty_paragraph_renders_with_br_placeholder() {
        let p = schema_basic::paragraph(Vec::new()).unwrap();
        let doc = schema_basic::doc(vec![p]).unwrap();
        assert_eq!(render_doc_to_html(&doc), r#"<p data-pos="0"><br/></p>"#,);
    }

    #[test]
    fn renders_nested_lists_with_increasing_positions() {
        let li = schema_basic::list_item(vec![schema_basic::paragraph(vec![schema_basic::text(
            "one",
            Vec::new(),
        )
        .unwrap()])
        .unwrap()])
        .unwrap();
        let ul = schema_basic::bullet_list(vec![li]).unwrap();
        let doc = schema_basic::doc(vec![ul]).unwrap();
        // ul at 0, li at 1 (inside ul.content), p at 2 (inside li.content).
        assert_eq!(
            render_doc_to_html(&doc),
            r#"<ul data-pos="0"><li data-pos="1"><p data-pos="2">one</p></li></ul>"#,
        );
    }

    #[test]
    fn renders_ordered_list_start_when_order_is_not_one() {
        let item =
            schema_basic::list_item(vec![schema_basic::paragraph(vec![schema_basic::text(
                "one",
                Vec::new(),
            )
            .unwrap()])
            .unwrap()])
            .unwrap();
        let mut attrs = Attrs::new();
        attrs.insert("order".to_string(), json!(3));
        let list = schema_basic::schema()
            .node("ordered_list", attrs, Fragment::from(item))
            .unwrap();
        let doc = schema_basic::doc(vec![list]).unwrap();
        assert_eq!(
            render_doc_to_html(&doc),
            r#"<ol data-pos="0" start="3"><li data-pos="1"><p data-pos="2">one</p></li></ol>"#,
        );
    }

    #[test]
    fn renders_image_and_link_attrs() {
        let link = schema_basic::link("https://example.test?a=1&b=2", Some("A \"title\"")).unwrap();
        let text = schema_basic::text("go", vec![link]).unwrap();
        let image =
            schema_basic::image("img\"<&.png", Some("Alt & text"), Some("Title <x>")).unwrap();
        let paragraph = schema_basic::paragraph(vec![text, image]).unwrap();
        let doc = schema_basic::doc(vec![paragraph]).unwrap();
        assert_eq!(
            render_doc_to_html(&doc),
            concat!(
                r#"<p data-pos="0"><a href="https://example.test?a=1&amp;b=2" "#,
                r#"title="A &quot;title&quot;">go</a>"#,
                r#"<img data-pos="3" src="img&quot;&lt;&amp;.png" "#,
                r#"alt="Alt &amp; text" title="Title &lt;x&gt;"/></p>"#
            ),
        );
    }

    #[test]
    fn node_view_registry_overrides_default_tag() {
        node_views::register("task_item", "pine-task-item");
        let item = schema_basic::task_item(
            true,
            vec![
                schema_basic::paragraph(vec![schema_basic::text("a", Vec::new()).unwrap()])
                    .unwrap(),
            ],
        )
        .unwrap();
        let list = schema_basic::task_list(vec![item]).unwrap();
        let doc = schema_basic::doc(vec![list]).unwrap();
        let html = render_doc_to_html(&doc);
        node_views::unregister("task_item");
        assert!(
            html.contains(r#"<pine-task-item data-pos="1" data-checked="true">"#),
            "expected the custom element with reflected attrs, got: {html}"
        );
        assert!(html.contains("</pine-task-item>"));
    }

    #[test]
    fn renders_task_list_with_checked_attribute() {
        let item_done = schema_basic::task_item(
            true,
            vec![
                schema_basic::paragraph(vec![schema_basic::text("done", Vec::new()).unwrap()])
                    .unwrap(),
            ],
        )
        .unwrap();
        let item_todo = schema_basic::task_item(
            false,
            vec![
                schema_basic::paragraph(vec![schema_basic::text("todo", Vec::new()).unwrap()])
                    .unwrap(),
            ],
        )
        .unwrap();
        let list = schema_basic::task_list(vec![item_done, item_todo]).unwrap();
        let doc = schema_basic::doc(vec![list]).unwrap();
        assert_eq!(
            render_doc_to_html(&doc),
            concat!(
                r#"<ul class="task-list" data-pos="0">"#,
                r#"<li class="task-item" data-pos="1" data-checked="true"><p data-pos="2">done</p></li>"#,
                r#"<li class="task-item" data-pos="9" data-checked="false"><p data-pos="10">todo</p></li>"#,
                "</ul>"
            ),
        );
    }
}
