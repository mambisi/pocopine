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
//! - `horizontal_rule` → `<hr>`, `image` → `<img>`, `hard_break` → `<br>`.
//! - `text` → escaped text content, marks wrap outside-in.
//!
//! Unknown node types fall back to `<span data-type="{name}">…</span>`
//! around their children so apps spot missing renderer cases without
//! losing data.

use crate::model::{Mark, Node};

pub mod dom_output;
pub mod dom_views;

pub use dom_output::{DomElementSpec, DomFragmentSpec, DomNodeSpec, DomOutputError};
pub use dom_views::{DomAttrBinding, DomOutputSpec, NodeDomSpec, NodeDomSpecError, UrlPolicy};

use crate::runtime::EditorRuntime;

/// Build the validated native host plan for a typed component view.
#[cfg(feature = "view")]
pub(crate) fn typed_node_view_host_spec(
    runtime: &EditorRuntime,
    node: &Node,
    outer_pos: usize,
) -> Option<dom_output::DomElementSpec> {
    let spec = runtime.lookup_typed_node_view(node.type_name())?;
    let mut host = if spec.host() == crate::view::NodeViewHost::Native {
        let native = runtime
            .lookup_dom_view(node.type_name())
            .expect("runtime validates native-host component views");
        native
            .root_element_spec(node)
            .expect("validated typed attrs project to the native host")
    } else {
        let inline = runtime
            .lookup_typed_node(node.type_name())
            .is_some_and(|typed| typed.spec().is_inline());
        dom_output::DomElementSpec::element(if inline { "span" } else { "div" })
            .expect("framework-selected typed host tag is valid")
    };
    host = host
        .attr("data-pos", outer_pos.to_string())
        .expect("framework data-pos attr is valid")
        .attr("data-pine-node-type", spec.node_type())
        .expect("framework semantic-type attr is valid")
        .attr("data-pine-node-view", "typed")
        .expect("framework node-view attr is valid");
    if matches!(spec.kind(), crate::view::NodeViewKind::Atom) {
        host = host
            .attr("contenteditable", "false")
            .expect("framework contenteditable attr is valid");
    }
    Some(host)
}

/// Stateless renderer scoped to a single [`EditorRuntime`]. The runtime
/// supplies per-runtime typed semantic DOM and component-view registries.
/// Component identity never comes from document data or a raw tag map.
///
/// All internal `render_*` helpers are methods on `Renderer` so they
/// share one source of truth for runtime bindings without threading
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
        self.compile(self.positioned_children(doc, 0, false))
    }

    /// Render the CHILDREN of `node` (no wrapper for `node` itself) at
    /// the given content-start position. Used by reconcilers that have
    /// located the DOM element for `node` and need to refresh just its
    /// inner contents.
    pub fn children(&self, node: &Node, content_start: usize) -> String {
        self.compile(self.positioned_children(node, content_start, true))
    }

    /// Render a single node (with its wrapper) at the given outer
    /// position. Used by reconcilers that want to swap one element out
    /// of a parent without re-rendering the parent's other children.
    pub fn one_node(&self, node: &Node, outer_pos: usize) -> String {
        self.compile(vec![self.one_node_plan(node, outer_pos)])
    }

    /// Build one validated structural node for direct browser materialization.
    pub(crate) fn one_node_plan(&self, node: &Node, outer_pos: usize) -> DomNodeSpec {
        self.node_plan(node, outer_pos)
    }

    fn compile(&self, nodes: Vec<DomNodeSpec>) -> String {
        let plan = DomFragmentSpec::new().extend(nodes);
        let mut output = String::new();
        plan.compile_into(&mut output)
            .expect("validated editor DOM plan serializes");
        output
    }

    fn positioned_children(
        &self,
        node: &Node,
        content_start: usize,
        empty_placeholder: bool,
    ) -> Vec<DomNodeSpec> {
        if empty_placeholder && node.content().is_empty() && !node.is_text() && !node.is_leaf() {
            return vec![element("br").into()];
        }
        let mut position = content_start;
        node.content()
            .iter()
            .map(|child| {
                let plan = self.node_plan(child, position);
                position = position.saturating_add(child.node_size());
                plan
            })
            .collect()
    }

    /// `outer_pos` is the model position right BEFORE `node` in its
    /// parent's content. The emitted element's `data-pos` matches
    /// that; callers (the selection bridge) treat content-start as
    /// `outer_pos + 1` for non-leaf nodes, or treat the leaf as a
    /// single position.
    fn node_plan(&self, node: &Node, outer_pos: usize) -> DomNodeSpec {
        if node.is_text() {
            return text_plan(node.text().unwrap_or(""), node.marks());
        }

        // Typed component views always start from an empty, stable native
        // host. The per-editor manager mounts the shell onto that exact host;
        // atom descendants belong to the component, while editable children
        // are rendered later into the compiled owned-content outlet.
        #[cfg(feature = "view")]
        if let Some(host) = typed_node_view_host_spec(self.runtime, node, outer_pos) {
            return host.into();
        }

        if let Some(spec) = self.runtime.lookup_dom_view(node.type_name()) {
            let content = self.positioned_children(node, outer_pos.saturating_add(1), true);
            return spec
                .editor_node(node, outer_pos, &content)
                .expect("runtime validated native DOM view");
        }

        match node.type_name() {
            "paragraph" => self.block_plan(node, "p", outer_pos),
            "blockquote" => self.block_plan(node, "blockquote", outer_pos),
            "bullet_list" => self.block_plan(node, "ul", outer_pos),
            "ordered_list" => self.ordered_list_plan(node, outer_pos),
            "list_item" => self.block_plan(node, "li", outer_pos),
            "task_list" => self.task_list_plan(node, outer_pos),
            "task_item" => self.task_item_plan(node, outer_pos),
            "heading" => {
                let level = node
                    .attrs()
                    .get("level")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .clamp(1, 6);
                let tag = ["h1", "h2", "h3", "h4", "h5", "h6"][level as usize - 1];
                self.block_plan(node, tag, outer_pos)
            }
            "code_block" => {
                let code = element("code")
                    .extend_nodes(self.positioned_children(
                        node,
                        outer_pos.saturating_add(1),
                        false,
                    ))
                    .expect("structural child append is infallible");
                element("pre")
                    .attr("data-pos", outer_pos.to_string())
                    .expect("framework data-pos attr is valid")
                    .child(code)
                    .expect("structural child append is infallible")
                    .into()
            }
            "horizontal_rule" => positioned_element("hr", outer_pos).into(),
            "hard_break" => positioned_element("br", outer_pos).into(),
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
                let mut image = positioned_element("img", outer_pos)
                    .attr("src", src)
                    .expect("framework image src attr is valid")
                    .attr("alt", alt)
                    .expect("framework image alt attr is valid");
                if let Some(title) = title {
                    image = image
                        .attr("title", title)
                        .expect("framework image title attr is valid");
                }
                image.into()
            }
            other => positioned_element("span", outer_pos)
                .attr("data-type", other)
                .expect("framework data-type attr is valid")
                .extend_nodes(self.positioned_children(node, outer_pos.saturating_add(1), false))
                .expect("structural child append is infallible")
                .into(),
        }
    }

    fn ordered_list_plan(&self, node: &Node, outer_pos: usize) -> DomNodeSpec {
        let mut list = positioned_element("ol", outer_pos);
        if let Some(order) = node.attrs().get("order").and_then(|v| v.as_i64())
            && order != 1
        {
            list = list
                .attr("start", order.to_string())
                .expect("framework ordered-list start attr is valid");
        }
        list.extend_nodes(self.positioned_children(node, outer_pos.saturating_add(1), true))
            .expect("structural child append is infallible")
            .into()
    }

    /// `task_list` is just a `<ul>` with a marker class. CSS hides the
    /// default bullet and styles items with a leading checkbox.
    fn task_list_plan(&self, node: &Node, outer_pos: usize) -> DomNodeSpec {
        positioned_element("ul", outer_pos)
            .attr("class", "task-list")
            .expect("framework task-list class attr is valid")
            .extend_nodes(self.positioned_children(node, outer_pos.saturating_add(1), true))
            .expect("structural child append is infallible")
            .into()
    }

    /// `task_item` is a `<li class="task-item">` with a `data-checked`
    /// attribute mirroring the model's `checked` attr.
    fn task_item_plan(&self, node: &Node, outer_pos: usize) -> DomNodeSpec {
        let checked = node
            .attrs()
            .get("checked")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        positioned_element("li", outer_pos)
            .attr("class", "task-item")
            .expect("framework task-item class attr is valid")
            .attr("data-checked", if checked { "true" } else { "false" })
            .expect("framework task-item checked attr is valid")
            .extend_nodes(self.positioned_children(node, outer_pos.saturating_add(1), true))
            .expect("structural child append is infallible")
            .into()
    }

    fn block_plan(&self, node: &Node, tag: &str, outer_pos: usize) -> DomNodeSpec {
        // Empty blocks retain a structural `<br>` placeholder so the editable
        // surface renders a clickable line. It has no `data-pos`; the bridge
        // treats it as the parent's content start.
        positioned_element(tag, outer_pos)
            .extend_nodes(self.positioned_children(node, outer_pos.saturating_add(1), true))
            .expect("structural child append is infallible")
            .into()
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

/// Build one node without crossing a serialized HTML boundary.
#[cfg(feature = "view")]
pub(crate) fn render_one_node_plan(
    runtime: &EditorRuntime,
    node: &Node,
    outer_pos: usize,
) -> DomNodeSpec {
    Renderer::new(runtime).one_node_plan(node, outer_pos)
}

fn element(tag: &str) -> DomElementSpec {
    DomElementSpec::element(tag).expect("framework-selected editor tag is valid")
}

fn positioned_element(tag: &str, outer_pos: usize) -> DomElementSpec {
    element(tag)
        .attr("data-pos", outer_pos.to_string())
        .expect("framework data-pos attr is valid")
}

fn text_plan(text: &str, marks: &[Mark]) -> DomNodeSpec {
    let mut output = DomNodeSpec::text(text);
    for mark in marks.iter().rev() {
        let mut wrapper = match mark.type_name() {
            "em" => element("em"),
            "strong" => element("strong"),
            "code" => element("code"),
            "link" => {
                let href = mark
                    .attrs()
                    .get("href")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let title = mark.attrs().get("title").and_then(|v| v.as_str());
                let mut link = element("a")
                    .attr("href", href)
                    .expect("framework link href attr is valid");
                if let Some(title) = title {
                    link = link
                        .attr("title", title)
                        .expect("framework link title attr is valid");
                }
                link
            }
            other => element("span")
                .attr("data-mark", other)
                .expect("framework data-mark attr is valid"),
        };
        wrapper = wrapper
            .node(output)
            .expect("structural child append is infallible");
        output = wrapper.into();
    }
    output
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
        assert_eq!(render(&doc), r#"<p data-pos="0"><br></p>"#,);
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
                r#"<img alt="Alt &amp; text" data-pos="3" "#,
                r#"src="img&quot;<&amp;.png" title="Title <x>"></p>"#
            ),
        );
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
                r#"<li class="task-item" data-checked="true" data-pos="1"><p data-pos="2">done</p></li>"#,
                r#"<li class="task-item" data-checked="false" data-pos="9"><p data-pos="10">todo</p></li>"#,
                "</ul>"
            ),
        );
    }
}
