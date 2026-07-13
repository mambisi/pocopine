use std::sync::Arc;

use pine_richtext::markdown::pulldown_cmark::{Alignment, Event, Tag, TagEnd};
use pine_richtext::markdown::{
    EventSink, MarkdownParseRule, NodeEmitter, ParseContext, ParseMapping, ParseMatch, TagKind,
};
use pine_richtext::model::{Attrs, Node};
use serde_json::json;

use super::TableAlignment;

pub(super) fn node_emitters() -> Vec<(String, NodeEmitter)> {
    vec![
        (
            "table".into(),
            Arc::new(
                |node: &Node, _parent: &Node, _index: usize, sink: &mut EventSink<'_>| {
                    let alignments = node
                        .child(0)
                        .map(|header| {
                            header
                                .content()
                                .iter()
                                .map(|cell| alignment_to_markdown(alignment_from_cell(cell)))
                                .collect()
                        })
                        .unwrap_or_default();
                    sink.push(Event::Start(Tag::Table(alignments)));
                    sink.render_content(node);
                    sink.push(Event::End(TagEnd::Table));
                },
            ),
        ),
        (
            "table_row".into(),
            Arc::new(
                |node: &Node, _parent: &Node, _index: usize, sink: &mut EventSink<'_>| {
                    let header = node.child_count() > 0
                        && node
                            .content()
                            .iter()
                            .all(|cell| cell.type_name() == "table_header_cell");
                    if header {
                        sink.push(Event::Start(Tag::TableHead));
                        sink.render_content(node);
                        sink.push(Event::End(TagEnd::TableHead));
                    } else {
                        sink.push(Event::Start(Tag::TableRow));
                        sink.render_content(node);
                        sink.push(Event::End(TagEnd::TableRow));
                    }
                },
            ),
        ),
        ("table_header_cell".into(), Arc::new(emit_cell)),
        ("table_cell".into(), Arc::new(emit_cell)),
    ]
}

fn emit_cell(node: &Node, _parent: &Node, _index: usize, sink: &mut EventSink<'_>) {
    sink.push(Event::Start(Tag::TableCell));
    sink.render_content(node);
    sink.push(Event::End(TagEnd::TableCell));
}

fn alignment_from_cell(node: &Node) -> Option<TableAlignment> {
    node.attrs()
        .get("alignment")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn alignment_to_markdown(value: Option<TableAlignment>) -> Alignment {
    match value {
        None => Alignment::None,
        Some(TableAlignment::Left) => Alignment::Left,
        Some(TableAlignment::Center) => Alignment::Center,
        Some(TableAlignment::Right) => Alignment::Right,
    }
}

fn alignment_to_attr(alignment: Alignment) -> Option<TableAlignment> {
    match alignment {
        Alignment::None => None,
        Alignment::Left => Some(TableAlignment::Left),
        Alignment::Center => Some(TableAlignment::Center),
        Alignment::Right => Some(TableAlignment::Right),
    }
}

pub(super) fn parse_rules() -> Vec<MarkdownParseRule> {
    vec![
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::Table),
            maps_to: ParseMapping::Block {
                node_type: "table".into(),
                get_attrs: None,
            },
        },
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::TableHead),
            maps_to: ParseMapping::Block {
                node_type: "table_row".into(),
                get_attrs: None,
            },
        },
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::TableRow),
            maps_to: ParseMapping::Block {
                node_type: "table_row".into(),
                get_attrs: None,
            },
        },
        MarkdownParseRule {
            matches: ParseMatch::Tag(TagKind::TableCell),
            maps_to: ParseMapping::ContextualBlock(Arc::new(
                |_event: &Event<'_>, context: &ParseContext<'_>| {
                    let node_type = if context.has_enclosing_tag(TagKind::TableHead) {
                        "table_header_cell"
                    } else {
                        "table_cell"
                    };
                    let column = context.child_count("table_row").unwrap_or(0);
                    let alignment = context
                        .opening_event("table")
                        .and_then(|event| match event {
                            Event::Start(Tag::Table(alignments)) => alignments.get(column).copied(),
                            _ => None,
                        })
                        .and_then(alignment_to_attr);
                    let mut attrs = Attrs::new();
                    if let Some(alignment) = alignment {
                        attrs.insert("alignment".into(), json!(alignment));
                    }
                    (node_type.to_string(), attrs)
                },
            )),
        },
    ]
}
