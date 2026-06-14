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
    //! Node-view spec type.
    //!
    //! **Phase 4b C5 cleanup:** the legacy process-global registry
    //! (`register`, `register_with_content`, `lookup`,
    //! `registered_tags`, `unregister`) has been **deleted**. Node-
    //! view bindings now live exclusively on the per-mount
    //! [`crate::runtime::EditorRuntime`] (via
    //! [`crate::runtime::NodeViewRegistry`]). Extensions contribute
    //! bindings through
    //! [`crate::extension::RichTextExtension::node_views`]; the
    //! runtime fold collects them at `RuntimeBuilder::build` time;
    //! the renderer / reconciler consult
    //! `runtime.lookup_node_view(node_type)` per render.
    //!
    //! This module is kept solely for the `NodeViewSpec` data type,
    //! which is re-exported from `runtime::node_views` for backward
    //! compatibility.

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
        /// leave this `None`.
        pub content_selector: Option<String>,
    }
}

use node_views::NodeViewSpec;

use crate::runtime::EditorRuntime;

pub(crate) fn attr_value_to_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Stateless renderer scoped to a single [`EditorRuntime`]. The runtime
/// supplies the per-instance node-view registry — two surfaces with
/// different runtimes may produce different HTML for the same `Node`
/// because each one resolves `task_item -> <custom-tag>` against its
/// own [`crate::runtime::NodeViewRegistry`].
///
/// All internal `render_*` helpers are methods on `Renderer` so they
/// share one source of truth for node-view bindings without threading
/// the runtime through every parameter list.
pub struct Renderer<'a> {
    runtime: &'a EditorRuntime,
}

impl<'a> Renderer<'a> {
    pub fn new(runtime: &'a EditorRuntime) -> Self {
        Self { runtime }
    }

    /// Walk `doc` and produce the matching HTML. The doc node itself
    /// isn't wrapped; its children are emitted directly so the caller
    /// can set the result on a surface's `innerHTML`. The surface
    /// element implicitly represents the doc and "owns" position 0.
    pub fn doc(&self, doc: &Node) -> String {
        let mut out = String::new();
        let mut pos = 0usize;
        for child in doc.content().iter() {
            self.render_node(child, pos, &mut out);
            pos += child.node_size();
        }
        out
    }

    /// Render the CHILDREN of `node` (no wrapper for `node` itself) at
    /// the given content-start position. Used by reconcilers that have
    /// located the DOM element for `node` and need to refresh just its
    /// inner contents.
    pub fn children(&self, node: &Node, content_start: usize) -> String {
        let mut out = String::new();
        if node.content().size() == 0 && !node.is_text() && !node.is_leaf() {
            out.push_str("<br/>");
            return out;
        }
        let mut pos = content_start;
        for child in node.content().iter() {
            self.render_node(child, pos, &mut out);
            pos += child.node_size();
        }
        out
    }

    /// Render a single node (with its wrapper) at the given outer
    /// position. Used by reconcilers that want to swap one element out
    /// of a parent without re-rendering the parent's other children.
    pub fn one_node(&self, node: &Node, outer_pos: usize) -> String {
        let mut out = String::new();
        self.render_node(node, outer_pos, &mut out);
        out
    }

    /// `outer_pos` is the model position right BEFORE `node` in its
    /// parent's content. The emitted element's `data-pos` matches
    /// that; callers (the selection bridge) treat content-start as
    /// `outer_pos + 1` for non-leaf nodes, or treat the leaf as a
    /// single position.
    fn render_node(&self, node: &Node, outer_pos: usize, out: &mut String) {
        if node.is_text() {
            let text = node.text().unwrap_or("");
            render_text(text, node.marks(), out);
            return;
        }

        // If a node view is registered for this node type IN THIS
        // RUNTIME, render it as the custom element instead of the
        // default tag. Two runtimes can disagree on which node_type
        // maps to which tag without colliding.
        if let Some(spec) = self.runtime.lookup_node_view(node.type_name()) {
            self.render_node_view(node, spec, outer_pos, out);
            return;
        }

        match node.type_name() {
            "paragraph" => self.render_block(node, "p", outer_pos, out),
            "blockquote" => self.render_block(node, "blockquote", outer_pos, out),
            "bullet_list" => self.render_block(node, "ul", outer_pos, out),
            "ordered_list" => self.render_ordered_list(node, outer_pos, out),
            "list_item" => self.render_block(node, "li", outer_pos, out),
            "task_list" => self.render_task_list(node, outer_pos, out),
            "task_item" => self.render_task_item(node, outer_pos, out),
            "heading" => {
                let level = node
                    .attrs()
                    .get("level")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .clamp(1, 6);
                let tag = format!("h{level}");
                self.render_block(node, &tag, outer_pos, out);
            }
            "code_block" => {
                out.push_str("<pre data-pos=\"");
                out.push_str(&outer_pos.to_string());
                out.push_str("\"><code>");
                for child in node.content().iter() {
                    self.render_node(child, outer_pos + 1, out);
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
                    self.render_node(child, child_pos, out);
                    child_pos += child.node_size();
                }
                out.push_str("</span>");
            }
        }
    }

    /// Emit a node as the custom element registered in this runtime's
    /// node-view registry. Every model attribute is mirrored onto the
    /// element as `data-<attr>=<json>` so the component can read it
    /// from the DOM. Children are rendered inline as content — the
    /// component is expected to lay them out (typically by just
    /// letting them render where they sit in its template / Shadow
    /// DOM).
    fn render_node_view(
        &self,
        node: &Node,
        spec: &NodeViewSpec,
        outer_pos: usize,
        out: &mut String,
    ) {
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
                self.render_node(child, child_pos, out);
                child_pos += child.node_size();
            }
        }
        out.push_str("</");
        out.push_str(&spec.tag);
        out.push('>');
    }

    fn render_ordered_list(&self, node: &Node, outer_pos: usize, out: &mut String) {
        out.push_str("<ol data-pos=\"");
        out.push_str(&outer_pos.to_string());
        out.push('"');
        if let Some(order) = node.attrs().get("order").and_then(|v| v.as_i64())
            && order != 1
        {
            out.push_str(" start=\"");
            out.push_str(&order.to_string());
            out.push('"');
        }
        out.push('>');
        if node.content().size() == 0 {
            out.push_str("<br/>");
        } else {
            let mut child_pos = outer_pos + 1;
            for child in node.content().iter() {
                self.render_node(child, child_pos, out);
                child_pos += child.node_size();
            }
        }
        out.push_str("</ol>");
    }

    /// `task_list` is just a `<ul>` with a marker class. CSS hides the
    /// default bullet and styles items with a leading checkbox.
    fn render_task_list(&self, node: &Node, outer_pos: usize, out: &mut String) {
        out.push_str("<ul class=\"task-list\" data-pos=\"");
        out.push_str(&outer_pos.to_string());
        out.push_str("\">");
        if node.content().size() == 0 {
            out.push_str("<br/>");
        } else {
            let mut child_pos = outer_pos + 1;
            for child in node.content().iter() {
                self.render_node(child, child_pos, out);
                child_pos += child.node_size();
            }
        }
        out.push_str("</ul>");
    }

    /// `task_item` is a `<li class="task-item">` with a `data-checked`
    /// attribute mirroring the model's `checked` attr.
    fn render_task_item(&self, node: &Node, outer_pos: usize, out: &mut String) {
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
                self.render_node(child, child_pos, out);
                child_pos += child.node_size();
            }
        }
        out.push_str("</li>");
    }

    fn render_block(&self, node: &Node, tag: &str, outer_pos: usize, out: &mut String) {
        out.push('<');
        out.push_str(tag);
        out.push_str(" data-pos=\"");
        out.push_str(&outer_pos.to_string());
        out.push_str("\">");
        if node.content().size() == 0 {
            // Empty blocks need a placeholder child so contentEditable
            // renders a clickable line. `<br/>` doesn't carry a `data-
            // pos` — the bridge treats it as if the cursor is at the
            // parent's content start (position `outer_pos + 1`).
            out.push_str("<br/>");
        } else {
            let mut child_pos = outer_pos + 1;
            for child in node.content().iter() {
                self.render_node(child, child_pos, out);
                child_pos += child.node_size();
            }
        }
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    }
}

/// Walk `doc` and produce the matching HTML. See [`Renderer::doc`].
pub fn render_doc_to_html(runtime: &EditorRuntime, doc: &Node) -> String {
    Renderer::new(runtime).doc(doc)
}

/// Render the CHILDREN of `node`. See [`Renderer::children`].
pub fn render_children_to_html(
    runtime: &EditorRuntime,
    node: &Node,
    content_start: usize,
) -> String {
    Renderer::new(runtime).children(node, content_start)
}

/// Render a single node with its wrapper. See [`Renderer::one_node`].
pub fn render_one_node_to_html(runtime: &EditorRuntime, node: &Node, outer_pos: usize) -> String {
    Renderer::new(runtime).one_node(node, outer_pos)
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

    /// Test helper: render `doc` through a freshly-built runtime
    /// with no extension overlay. Builds its own runtime (not the
    /// cached `runtime::registry::default()`) so render tests don't
    /// race with `extension::tests` / `runtime::tests` on the
    /// shared `SCHEMA_REALIZED` flag. Equivalent extension chain
    /// since `RuntimeBuilder::new()` defaults to
    /// `default_extensions()`.
    fn render(doc: &Node) -> String {
        let runtime = crate::runtime::RuntimeBuilder::new().build();
        render_doc_to_html(&runtime, doc)
    }

    #[test]
    fn paragraphs_carry_their_outer_pos_in_data_pos() {
        let p1 =
            schema_basic::paragraph(vec![schema_basic::text("foo", Vec::new()).unwrap()]).unwrap();
        let p2 =
            schema_basic::paragraph(vec![schema_basic::text("bar", Vec::new()).unwrap()]).unwrap();
        let doc = schema_basic::doc(vec![p1, p2]).unwrap();
        // p1.node_size = 1 + 3 + 1 = 5 → second paragraph at outer pos 5.
        assert_eq!(
            render(&doc),
            r#"<p data-pos="0">foo</p><p data-pos="5">bar</p>"#,
        );
    }

    #[test]
    fn renders_heading_with_level_and_pos() {
        let h = schema_basic::heading(2, vec![schema_basic::text("Title", Vec::new()).unwrap()])
            .unwrap();
        let doc = schema_basic::doc(vec![h]).unwrap();
        assert_eq!(render(&doc), r#"<h2 data-pos="0">Title</h2>"#,);
    }

    #[test]
    fn wraps_marked_text_outside_in() {
        let em = schema_basic::em().unwrap();
        let strong = schema_basic::strong().unwrap();
        let marked = schema_basic::text("bold-em", vec![em.clone(), strong.clone()]).unwrap();
        let p = schema_basic::paragraph(vec![marked]).unwrap();
        let doc = schema_basic::doc(vec![p]).unwrap();
        assert_eq!(
            render(&doc),
            r#"<p data-pos="0"><em><strong>bold-em</strong></em></p>"#,
        );
    }

    #[test]
    fn escapes_html_in_text() {
        let p = schema_basic::paragraph(vec![
            schema_basic::text("a < b & c > d", Vec::new()).unwrap(),
        ])
        .unwrap();
        let doc = schema_basic::doc(vec![p]).unwrap();
        assert_eq!(
            render(&doc),
            r#"<p data-pos="0">a &lt; b &amp; c &gt; d</p>"#,
        );
    }

    #[test]
    fn empty_paragraph_renders_with_br_placeholder() {
        let p = schema_basic::paragraph(Vec::new()).unwrap();
        let doc = schema_basic::doc(vec![p]).unwrap();
        assert_eq!(render(&doc), r#"<p data-pos="0"><br/></p>"#,);
    }

    #[test]
    fn renders_nested_lists_with_increasing_positions() {
        let li = schema_basic::list_item(vec![
            schema_basic::paragraph(vec![schema_basic::text("one", Vec::new()).unwrap()]).unwrap(),
        ])
        .unwrap();
        let ul = schema_basic::bullet_list(vec![li]).unwrap();
        let doc = schema_basic::doc(vec![ul]).unwrap();
        // ul at 0, li at 1 (inside ul.content), p at 2 (inside li.content).
        assert_eq!(
            render(&doc),
            r#"<ul data-pos="0"><li data-pos="1"><p data-pos="2">one</p></li></ul>"#,
        );
    }

    #[test]
    fn renders_ordered_list_start_when_order_is_not_one() {
        let item = schema_basic::list_item(vec![
            schema_basic::paragraph(vec![schema_basic::text("one", Vec::new()).unwrap()]).unwrap(),
        ])
        .unwrap();
        let mut attrs = Attrs::new();
        attrs.insert("order".to_string(), json!(3));
        let list = schema_basic::schema()
            .node("ordered_list", attrs, Fragment::from(item))
            .unwrap();
        let doc = schema_basic::doc(vec![list]).unwrap();
        assert_eq!(
            render(&doc),
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
            render(&doc),
            concat!(
                r#"<p data-pos="0"><a href="https://example.test?a=1&amp;b=2" "#,
                r#"title="A &quot;title&quot;">go</a>"#,
                r#"<img data-pos="3" src="img&quot;&lt;&amp;.png" "#,
                r#"alt="Alt &amp; text" title="Title &lt;x&gt;"/></p>"#
            ),
        );
    }

    #[test]
    fn runtime_node_view_overrides_default_tag() {
        use crate::extension::{ExtensionNodeView, RichTextExtension};
        use crate::runtime::RuntimeBuilder;

        struct TaskItemView;
        impl RichTextExtension for TaskItemView {
            fn name(&self) -> &str {
                "test-task-item-view"
            }
            fn node_views(&self) -> Vec<ExtensionNodeView> {
                vec![ExtensionNodeView {
                    node_type: "task_item".into(),
                    tag: "pine-task-item".into(),
                    content_selector: None,
                }]
            }
        }

        let runtime = RuntimeBuilder::new().with(TaskItemView).build();
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
        let html = render_doc_to_html(&runtime, &doc);
        assert!(
            html.contains(r#"<pine-task-item data-pos="1" data-checked="true">"#),
            "expected the custom element with reflected attrs, got: {html}"
        );
        assert!(html.contains("</pine-task-item>"));
    }

    #[test]
    fn two_runtimes_render_same_doc_with_different_node_view_tags() {
        use crate::extension::{ExtensionNodeView, RichTextExtension};
        use crate::runtime::RuntimeBuilder;

        struct TaskItemViewA;
        impl RichTextExtension for TaskItemViewA {
            fn name(&self) -> &str {
                "view-a"
            }
            fn node_views(&self) -> Vec<ExtensionNodeView> {
                vec![ExtensionNodeView {
                    node_type: "task_item".into(),
                    tag: "tag-a-item".into(),
                    content_selector: None,
                }]
            }
        }
        struct TaskItemViewB;
        impl RichTextExtension for TaskItemViewB {
            fn name(&self) -> &str {
                "view-b"
            }
            fn node_views(&self) -> Vec<ExtensionNodeView> {
                vec![ExtensionNodeView {
                    node_type: "task_item".into(),
                    tag: "tag-b-item".into(),
                    content_selector: None,
                }]
            }
        }

        let item = schema_basic::task_item(
            false,
            vec![
                schema_basic::paragraph(vec![schema_basic::text("hi", Vec::new()).unwrap()])
                    .unwrap(),
            ],
        )
        .unwrap();
        let list = schema_basic::task_list(vec![item]).unwrap();
        let doc = schema_basic::doc(vec![list]).unwrap();

        let rt_a = RuntimeBuilder::new().with(TaskItemViewA).build();
        let rt_b = RuntimeBuilder::new().with(TaskItemViewB).build();

        let html_a = render_doc_to_html(&rt_a, &doc);
        let html_b = render_doc_to_html(&rt_b, &doc);

        assert!(html_a.contains("<tag-a-item"), "A: {html_a}");
        assert!(html_a.contains("</tag-a-item>"));
        assert!(html_b.contains("<tag-b-item"), "B: {html_b}");
        assert!(html_b.contains("</tag-b-item>"));
        assert_ne!(html_a, html_b);
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
            render(&doc),
            concat!(
                r#"<ul class="task-list" data-pos="0">"#,
                r#"<li class="task-item" data-pos="1" data-checked="true"><p data-pos="2">done</p></li>"#,
                r#"<li class="task-item" data-pos="9" data-checked="false"><p data-pos="10">todo</p></li>"#,
                "</ul>"
            ),
        );
    }
}
