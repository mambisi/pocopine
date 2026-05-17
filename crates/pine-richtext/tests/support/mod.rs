// Shared by every integration-test binary in this crate. Each binary uses
// a different subset of these helpers, so dead-code warnings would fire on
// any helper that one binary happens not to call. Silence them at the
// module level rather than tagging every function individually.
#![allow(dead_code)]

use std::collections::BTreeMap;

use pine_richtext::model::{Attrs, ContentExpr, Fragment, Mark, Node, NodeSpec, Schema, Slice};
use pine_richtext::schema_basic;
use pine_richtext::state::{EditorState, EditorStateConfig};
use pine_richtext::transform::{MapResult, Mapping, MarkStep, ReplaceStep, Step};

pub fn text(value: &str) -> Node {
    schema_basic::text(value, Vec::new()).unwrap()
}

pub fn marked_text(value: &str, marks: Vec<Mark>) -> Node {
    schema_basic::text(value, marks).unwrap()
}

pub fn paragraph(children: Vec<Node>) -> Node {
    schema_basic::paragraph(children).unwrap()
}

pub fn paragraph_text(value: &str) -> Node {
    paragraph(vec![text(value)])
}

pub fn empty_paragraph() -> Node {
    paragraph(Vec::new())
}

pub fn heading(level: u8, value: &str) -> Node {
    schema_basic::heading(level, vec![text(value)]).unwrap()
}

pub fn code_block(value: &str) -> Node {
    schema_basic::code_block(value).unwrap()
}

pub fn horizontal_rule() -> Node {
    schema_basic::horizontal_rule().unwrap()
}

pub fn image(src: &str) -> Node {
    schema_basic::image(src, Option::<String>::None, Option::<String>::None).unwrap()
}

pub fn hard_break() -> Node {
    schema_basic::hard_break().unwrap()
}

pub fn list_item(children: Vec<Node>) -> Node {
    schema_basic::list_item(children).unwrap()
}

pub fn list_item_text(value: &str) -> Node {
    list_item(vec![paragraph_text(value)])
}

pub fn bullet_list(items: Vec<Node>) -> Node {
    schema_basic::bullet_list(items).unwrap()
}

pub fn ordered_list(items: Vec<Node>) -> Node {
    schema_basic::ordered_list(items).unwrap()
}

pub fn doc(blocks: Vec<Node>) -> Node {
    schema_basic::doc(blocks).unwrap()
}

pub fn state_with_doc(doc: Node) -> EditorState {
    EditorState::create(EditorStateConfig::new(schema_basic::schema(), doc)).unwrap()
}

pub fn starter_state() -> EditorState {
    state_with_doc(doc(vec![paragraph_text("ok")]))
}

#[derive(Clone, Debug)]
pub struct TaggedNode {
    pub node: Node,
    pub tags: BTreeMap<String, usize>,
}

impl TaggedNode {
    pub fn tag(&self, name: &str) -> usize {
        *self
            .tags
            .get(name)
            .unwrap_or_else(|| panic!("missing tag {name}"))
    }
}

pub enum TaggedChild {
    Node(TaggedNode),
    Tag(String),
}

impl From<TaggedNode> for TaggedChild {
    fn from(value: TaggedNode) -> Self {
        Self::Node(value)
    }
}

pub fn tag(name: &str) -> TaggedChild {
    TaggedChild::Tag(name.to_string())
}

pub fn tagged_text(value: &str) -> TaggedNode {
    let (text_value, tags) = parse_tagged_text(value);
    TaggedNode {
        node: text(&text_value),
        tags,
    }
}

pub fn tagged_marked_text(value: &str, marks: Vec<Mark>) -> TaggedNode {
    let (text_value, tags) = parse_tagged_text(value);
    TaggedNode {
        node: marked_text(&text_value, marks),
        tags,
    }
}

pub fn tagged_paragraph(children: Vec<TaggedChild>) -> TaggedNode {
    tagged_parent(children, paragraph)
}

pub fn tagged_paragraph_text(value: &str) -> TaggedNode {
    tagged_paragraph(vec![tagged_text(value).into()])
}

pub fn tagged_heading_text(level: u8, value: &str) -> TaggedNode {
    tagged_parent(vec![tagged_text(value).into()], |nodes| {
        schema_basic::heading(level, nodes).unwrap()
    })
}

pub fn tagged_blockquote(children: Vec<TaggedChild>) -> TaggedNode {
    tagged_parent(children, |nodes| schema_basic::blockquote(nodes).unwrap())
}

pub fn tagged_list_item(children: Vec<TaggedChild>) -> TaggedNode {
    tagged_parent(children, |nodes| schema_basic::list_item(nodes).unwrap())
}

pub fn tagged_list_item_text(value: &str) -> TaggedNode {
    tagged_list_item(vec![tagged_paragraph_text(value).into()])
}

pub fn tagged_bullet_list(items: Vec<TaggedChild>) -> TaggedNode {
    tagged_parent(items, |nodes| schema_basic::bullet_list(nodes).unwrap())
}

pub fn tagged_ordered_list(items: Vec<TaggedChild>) -> TaggedNode {
    tagged_parent(items, |nodes| schema_basic::ordered_list(nodes).unwrap())
}

pub fn tagged_doc(children: Vec<TaggedChild>) -> TaggedNode {
    tagged_parent(children, doc)
}

pub fn tagged_node(node: Node) -> TaggedNode {
    TaggedNode {
        node,
        tags: BTreeMap::new(),
    }
}

pub fn tagged_horizontal_rule() -> TaggedNode {
    tagged_node(horizontal_rule())
}

pub fn tagged_hard_break() -> TaggedNode {
    tagged_node(hard_break())
}

pub fn tagged_image(src: &str) -> TaggedNode {
    tagged_node(image(src))
}

pub fn tagged_code_block(value: &str) -> TaggedNode {
    tagged_node(code_block(value))
}

fn tagged_parent(children: Vec<TaggedChild>, build: impl FnOnce(Vec<Node>) -> Node) -> TaggedNode {
    let mut nodes = Vec::new();
    let mut tags = BTreeMap::new();
    let mut offset = 0;

    for child in children {
        match child {
            TaggedChild::Tag(name) => {
                tags.insert(name, offset);
            }
            TaggedChild::Node(child) => {
                let content_start = if child.node.is_text() || child.node.is_leaf() {
                    offset
                } else {
                    offset + 1
                };
                for (name, pos) in child.tags {
                    tags.insert(name, content_start + pos);
                }
                offset += child.node.node_size();
                nodes.push(child.node);
            }
        }
    }

    TaggedNode {
        node: build(nodes),
        tags,
    }
}

fn parse_tagged_text(value: &str) -> (String, BTreeMap<String, usize>) {
    let mut text = String::new();
    let mut tags = BTreeMap::new();
    let mut chars = value.chars().peekable();
    let mut pos = 0;

    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut name = String::new();
            for next in chars.by_ref() {
                if next == '>' {
                    break;
                }
                name.push(next);
            }
            if name.is_empty() {
                text.push(ch);
                pos += 1;
            } else {
                tags.insert(name, pos);
            }
        } else {
            text.push(ch);
            pos += 1;
        }
    }

    (text, tags)
}

// ====================================================================
// Parity-test helpers (moved here from prosemirror_parity.rs during the
// split into parity_model.rs / parity_transform.rs / parity_state.rs).
// Each split test binary uses a subset; `#[allow(dead_code)]` keeps the
// build clean across all binaries.
// ====================================================================

#[allow(dead_code)]
pub fn text_content(node: Option<Node>) -> Option<String> {
    node.map(|node| node.text_content())
}

#[allow(dead_code)]
pub fn assert_slice(
    tagged: &TaggedNode,
    from_tag: Option<&str>,
    to_tag: Option<&str>,
    expected: &Node,
    open_start: usize,
    open_end: usize,
) {
    let from = from_tag.map_or(0, |tag| tagged.tag(tag));
    let to = to_tag.map_or_else(|| tagged.node.content_size(), |tag| tagged.tag(tag));
    let slice = tagged.node.slice(from, to).unwrap();
    assert_eq!(&slice.content, expected.content());
    assert_eq!(slice.open_start, open_start);
    assert_eq!(slice.open_end, open_end);
}

#[allow(dead_code)]
pub fn assert_between(
    doc: &Node,
    from: usize,
    to: usize,
    expected: &[(&str, usize, Option<&str>, usize)],
) {
    let mut found = Vec::new();
    doc.nodes_between(from, to, |node, pos, parent, index| {
        assert_eq!(doc.node_at(pos).unwrap(), Some(node));
        found.push((
            node.text().unwrap_or_else(|| node.type_name()).to_string(),
            pos,
            parent.map(|node| node.type_name().to_string()),
            index,
        ));
        true
    })
    .unwrap();

    let expected = expected
        .iter()
        .map(|(name, pos, parent, index)| {
            (
                (*name).to_string(),
                *pos,
                parent.map(str::to_string),
                *index,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(found, expected);
}

#[allow(dead_code)]
pub fn map_flags(result: MapResult) -> String {
    let mut flags = String::new();
    if result.deleted {
        flags.push('d');
    }
    if result.deleted_before {
        flags.push('b');
    }
    if result.deleted_after {
        flags.push('a');
    }
    if result.deleted_across {
        flags.push('x');
    }
    flags
}

#[allow(dead_code)]
pub fn assert_mapping(mapping: &Mapping, cases: &[(usize, usize, i8, bool)]) {
    let inverted = mapping.invert();
    for &(from, to, assoc, lossy) in cases {
        assert_eq!(mapping.map(from, assoc), to, "mapping {from} with {assoc}");
        if !lossy {
            assert_eq!(
                inverted.map(to, assoc),
                from,
                "inverted mapping {to} with {assoc}"
            );
        }
    }
}

#[allow(dead_code)]
pub fn heading_with_schema(schema: &Schema, value: &str) -> Node {
    schema
        .node(
            "heading",
            Attrs::new(),
            Fragment::from(schema.text(value, Vec::new()).unwrap()),
        )
        .unwrap()
}

#[allow(dead_code)]
pub fn paragraph_with_schema(schema: &Schema, value: &str) -> Node {
    schema
        .node(
            "paragraph",
            Attrs::new(),
            Fragment::from(schema.text(value, Vec::new()).unwrap()),
        )
        .unwrap()
}

#[allow(dead_code)]
pub fn content_match_schema() -> Schema {
    Schema::builder()
        .node(NodeSpec::new("doc").content("block*"))
        .node(NodeSpec::new("paragraph").group("block").content("inline*"))
        .node(NodeSpec::new("heading").group("block").content("inline*"))
        .node(NodeSpec::new("code_block").group("block").content("text*"))
        .node(NodeSpec::new("horizontal_rule").group("block"))
        .node(NodeSpec::new("text").group("inline").inline())
        .node(NodeSpec::new("hard_break").group("inline").inline().atom())
        .node(NodeSpec::new("image").group("inline").inline().atom())
        .finish()
        .unwrap()
}

#[allow(dead_code)]
pub fn fragment_for_types(schema: &Schema, names: &[&str]) -> Fragment {
    Fragment::from(
        names
            .iter()
            .map(|name| schema.default_node(name).unwrap())
            .collect::<Vec<_>>(),
    )
}

#[allow(dead_code)]
pub fn assert_fill_before(
    schema: &Schema,
    expr: &str,
    before: &[&str],
    after: &[&str],
    to_end: bool,
    expected: Option<&[&str]>,
) {
    let expr = ContentExpr::parse(expr).unwrap();
    let before = fragment_for_types(schema, before);
    let after = fragment_for_types(schema, after);
    let fill = expr
        .match_fragment(schema, &before)
        .unwrap()
        .fill_before(&after, to_end);

    match (fill, expected) {
        (Some(fill), Some(expected)) => assert_fragment_types(&fill, expected),
        (None, None) => {}
        (Some(fill), None) => panic!("unexpected fill {:?}", fragment_type_names(&fill)),
        (None, Some(expected)) => panic!("missing fill {:?}", expected),
    }
}

#[allow(dead_code)]
pub fn assert_fragment_types(fragment: &Fragment, expected: &[&str]) {
    assert_eq!(
        fragment_type_names(fragment),
        expected
            .iter()
            .map(|name| (*name).to_string())
            .collect::<Vec<_>>()
    );
}

#[allow(dead_code)]
pub fn fragment_type_names(fragment: &Fragment) -> Vec<String> {
    fragment
        .iter()
        .map(|node| node.type_name().to_string())
        .collect()
}

#[allow(dead_code)]
pub fn replace_step(from: usize, to: usize, text: Option<&str>) -> Step {
    let slice = text.map_or_else(Slice::empty, |value| {
        Slice::new(
            Fragment::from(schema_basic::text(value, Vec::new()).unwrap()),
            0,
            0,
        )
    });
    Step::Replace(ReplaceStep::new(from, to, slice))
}

#[allow(dead_code)]
pub fn add_em_step(from: usize, to: usize) -> Step {
    Step::AddMark(MarkStep::new(from, to, schema_basic::em().unwrap()))
}

#[allow(dead_code)]
pub fn remove_em_step(from: usize, to: usize) -> Step {
    Step::RemoveMark(MarkStep::new(from, to, schema_basic::em().unwrap()))
}

#[allow(dead_code)]
pub fn assert_steps_merge(first: Step, second: Step) {
    let schema = schema_basic::schema();
    let before = doc(vec![paragraph_text("foobar")]);
    let merged = first.merge(&second).expect("steps should merge");
    let after_first = first.apply(&before, &schema).unwrap().doc;
    let after_second = second.apply(&after_first, &schema).unwrap().doc;
    let after_merged = merged.apply(&before, &schema).unwrap().doc;
    assert_eq!(after_merged, after_second);
}

#[allow(dead_code)]
pub fn assert_steps_do_not_merge(first: Step, second: Step) {
    assert!(first.merge(&second).is_none());
}

#[allow(dead_code)]
pub fn assert_replace_error(target: TaggedNode, insert: Option<TaggedNode>, expected: &str) {
    let slice = insert.map_or_else(Slice::empty, |insert| {
        insert.node.slice(insert.tag("a"), insert.tag("b")).unwrap()
    });
    let from = target.tag("a");
    let to = target.tag("b");
    let mut tr = pine_richtext::transform::Transform::new(schema_basic::schema(), target.node);
    let err = tr.replace(from, to, slice).unwrap_err().to_string();
    assert!(
        err.to_lowercase().contains(expected),
        "expected error containing {expected:?}, got {err:?}"
    );
}
