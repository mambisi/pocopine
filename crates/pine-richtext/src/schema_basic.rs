use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::model::{Attrs, Fragment, Mark, MarkPolicy, MarkSpec, Node, NodeSpec, Schema, Whitespace};
use crate::RichTextResult;

/// Return the basic rich text schema.
pub fn schema() -> Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            Schema::builder()
                .node(NodeSpec::new("doc").content("block+"))
                .node(NodeSpec::new("paragraph").group("block").content("inline*"))
                .node(
                    NodeSpec::new("blockquote")
                        .group("block")
                        .content("block+")
                        .defining_for_content(),
                )
                .node(NodeSpec::new("horizontal_rule").group("block").atom())
                .node(NodeSpec::new("heading").group("block").content("inline*"))
                .node(
                    NodeSpec::new("code_block")
                        .group("block")
                        .content("text*")
                        .marks(MarkPolicy::None)
                        .code()
                        .whitespace(Whitespace::Pre)
                        .defining_for_content(),
                )
                .node(
                    NodeSpec::new("bullet_list")
                        .group("block")
                        .content("list_item+"),
                )
                .node(
                    NodeSpec::new("ordered_list")
                        .group("block")
                        .content("list_item+"),
                )
                .node(
                    NodeSpec::new("list_item")
                        .content("paragraph block*")
                        .defining(),
                )
                .node(NodeSpec::new("text").group("inline").inline())
                .node(
                    NodeSpec::new("image")
                        .group("inline")
                        .inline()
                        .atom()
                        .marks(MarkPolicy::None),
                )
                .node(NodeSpec::new("hard_break").group("inline").inline().atom())
                .mark(MarkSpec::new("link").inclusive(false))
                .mark(MarkSpec::new("em"))
                .mark(MarkSpec::new("strong"))
                .mark(MarkSpec::new("code").excludes("_"))
                .finish()
                .expect("basic schema is valid")
        })
        .clone()
}

/// Alias for [`schema`].
pub fn basic_schema() -> Schema {
    schema()
}

/// Build a document node.
pub fn doc(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("doc", Attrs::new(), Fragment::from(children))
}

/// Build a paragraph node.
pub fn paragraph(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("paragraph", Attrs::new(), Fragment::from(children))
}

/// Build a blockquote node.
pub fn blockquote(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("blockquote", Attrs::new(), Fragment::from(children))
}

/// Build a horizontal rule node.
pub fn horizontal_rule() -> RichTextResult<Node> {
    schema().node("horizontal_rule", Attrs::new(), Fragment::empty())
}

/// Build a heading node with a numeric level attribute.
pub fn heading(level: u8, children: Vec<Node>) -> RichTextResult<Node> {
    let mut attrs = Attrs::new();
    attrs.insert("level".to_string(), json!(level.clamp(1, 6)));
    schema().node("heading", attrs, Fragment::from(children))
}

/// Build a code block. The text is stored as plain `text` children.
pub fn code_block(text: impl Into<String>) -> RichTextResult<Node> {
    let text = text.into();
    let content = if text.is_empty() {
        Fragment::empty()
    } else {
        Fragment::from(schema().text(text, Vec::new())?)
    };
    schema().node("code_block", Attrs::new(), content)
}

/// Build a text node.
pub fn text(value: impl Into<String>, marks: Vec<Mark>) -> RichTextResult<Node> {
    schema().text(value, marks)
}

/// Build an image node.
pub fn image(
    src: impl Into<String>,
    alt: Option<impl Into<String>>,
    title: Option<impl Into<String>>,
) -> RichTextResult<Node> {
    let mut attrs = Attrs::new();
    attrs.insert("src".to_string(), Value::String(src.into()));
    if let Some(alt) = alt {
        attrs.insert("alt".to_string(), Value::String(alt.into()));
    }
    if let Some(title) = title {
        attrs.insert("title".to_string(), Value::String(title.into()));
    }
    schema().node("image", attrs, Fragment::empty())
}

/// Build a hard break node.
pub fn hard_break() -> RichTextResult<Node> {
    schema().node("hard_break", Attrs::new(), Fragment::empty())
}

/// Build a list item node.
pub fn list_item(children: Vec<Node>) -> RichTextResult<Node> {
    schema().node("list_item", Attrs::new(), Fragment::from(children))
}

/// Build a bullet list node.
pub fn bullet_list(items: Vec<Node>) -> RichTextResult<Node> {
    schema().node("bullet_list", Attrs::new(), Fragment::from(items))
}

/// Build an ordered list node.
pub fn ordered_list(items: Vec<Node>) -> RichTextResult<Node> {
    schema().node("ordered_list", Attrs::new(), Fragment::from(items))
}

/// Build a link mark.
pub fn link(href: impl Into<String>, title: Option<impl Into<String>>) -> RichTextResult<Mark> {
    let mut attrs = Attrs::new();
    attrs.insert("href".to_string(), Value::String(href.into()));
    if let Some(title) = title {
        attrs.insert("title".to_string(), Value::String(title.into()));
    }
    schema().mark("link", attrs)
}

/// Build an emphasis mark.
pub fn em() -> RichTextResult<Mark> {
    schema().mark("em", Attrs::new())
}

/// Build a strong mark.
pub fn strong() -> RichTextResult<Mark> {
    schema().mark("strong", Attrs::new())
}

/// Build a code mark.
pub fn code() -> RichTextResult<Mark> {
    schema().mark("code", Attrs::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_builders_create_valid_document() {
        let strong = strong().unwrap();
        let document = doc(vec![
            heading(2, vec![text("Title", Vec::new()).unwrap()]).unwrap(),
            paragraph(vec![
                text("Hello ", Vec::new()).unwrap(),
                text("world", vec![strong]).unwrap(),
                hard_break().unwrap(),
            ])
            .unwrap(),
        ])
        .unwrap();

        schema().check_node(&document).unwrap();
        assert_eq!(document.text_content(), "TitleHello world");
    }

    #[test]
    fn basic_schema_rejects_blocks_inside_paragraph() {
        let nested = blockquote(vec![paragraph(Vec::new()).unwrap()]).unwrap();
        let err = paragraph(vec![nested]).unwrap_err();
        assert!(err.to_string().contains("paragraph cannot contain"));
    }

    #[test]
    fn leaf_and_empty_container_sizes_are_distinct() {
        let paragraph = paragraph(Vec::new()).unwrap();
        let hard_break = hard_break().unwrap();

        assert!(!paragraph.is_leaf());
        assert!(hard_break.is_leaf());
        assert_eq!(paragraph.node_size(), 2);
        assert_eq!(hard_break.node_size(), 1);

        let decoded: Node =
            serde_json::from_value(serde_json::to_value(&hard_break).unwrap()).unwrap();
        assert_eq!(decoded, hard_break);
        schema().check_node(&decoded).unwrap();
    }
}
