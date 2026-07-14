//! Typed semantic GFM tables for `pine-richtext`.
//!
//! The document owns rows, cells, alignment, column widths, and row heights.
//! Markdown preserves the rectangular cell text and a single alignment per
//! column. It cannot represent pixel dimensions, so `column_widths` and row
//! `height` intentionally reset to defaults after a Markdown round trip.
//! Per-cell alignment differences within one column are also reduced to the
//! header cell's alignment because GFM's delimiter row is column-scoped.
//!
//! This module is model-only. Resize handles, menus, rectangular selections,
//! and component/node-view rendering belong to later view layers.

mod commands;
mod dom;
mod map;
mod markdown;

#[cfg(feature = "table-view")]
pub mod view;

use pine_richtext::extension::{KeyBindings, NamedCommand, RichTextExtension};
use pine_richtext::model::{MarkPolicy, NodeSpec, TableRole};
use pine_richtext::render::{DomAttrBinding, DomOutputSpec, NodeDomSpec};
use pine_richtext::serialization::{
    ClipboardPolicy, MarkdownPolicy, NodeSerializationSpec, PlainTextPolicy, SemanticHtmlPolicy,
    TextProjection,
};
use pine_richtext::{RichTextNodeAttrs, RichTextNodeType, TypedNodeSpec};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[cfg(feature = "table-view")]
pub use commands::{MoveColumn, MoveRow, ResizeColumn, ResizeRow, SelectCells};
pub use commands::{
    delete_column, delete_row, delete_table, go_to_next_cell, go_to_previous_cell,
    insert_column_after, insert_column_before, insert_row_after, insert_row_before, insert_table,
    move_column, move_row, set_cell_alignment, set_cell_alignment_at, set_column_width,
    set_column_width_at, set_row_height, set_row_height_at,
};
pub use dom::{
    TABLE_CELL_ATTR, TABLE_CELL_CLASS, TABLE_HEADER_CELL_CLASS, TABLE_ROW_ATTR, TABLE_ROW_CLASS,
    TABLE_SELECTED_ATTR,
};
pub use map::{CellRect, TableMap, TableMapError};

pub const MIN_COLUMN_WIDTH: u32 = 40;
pub const MAX_COLUMN_WIDTH: u32 = 2_000;
pub const MIN_ROW_HEIGHT: u32 = 24;
pub const MAX_ROW_HEIGHT: u32 = 1_000;
pub const MAX_TABLE_ROWS: usize = 1_000;
pub const MAX_TABLE_COLUMNS: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableAlignment {
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
pub struct TableAttrs {
    /// Optional pixel width for each logical column. Missing/trailing entries
    /// mean automatic layout.
    #[serde(default)]
    pub column_widths: Vec<Option<u32>>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
pub struct TableRowAttrs {
    /// Persisted minimum row height in pixels.
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
pub struct TableHeaderCellAttrs {
    #[serde(default)]
    pub alignment: Option<TableAlignment>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
pub struct TableCellAttrs {
    #[serde(default)]
    pub alignment: Option<TableAlignment>,
}

pub struct TableNode;
pub struct TableRowNode;
pub struct TableHeaderCellNode;
pub struct TableCellNode;

impl RichTextNodeType for TableNode {
    const NAME: &'static str = "table";
    const VERSION: u32 = 1;
    type Attrs = TableAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("block")
            .table_role(TableRole::Table)
            .content("table_row+")
            .defining()
            .isolating()
            .attr("column_widths", json!([]))
    }
}

impl RichTextNodeType for TableRowNode {
    const NAME: &'static str = "table_row";
    const VERSION: u32 = 1;
    type Attrs = TableRowAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .table_role(TableRole::Row)
            .content("(table_header_cell | table_cell)+")
            .attr("height", serde_json::Value::Null)
    }
}

impl RichTextNodeType for TableHeaderCellNode {
    const NAME: &'static str = "table_header_cell";
    const VERSION: u32 = 1;
    type Attrs = TableHeaderCellAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .table_role(TableRole::Cell)
            .content("inline*")
            .marks(MarkPolicy::All)
            .defining()
            .isolating()
            .attr("alignment", serde_json::Value::Null)
    }
}

impl RichTextNodeType for TableCellNode {
    const NAME: &'static str = "table_cell";
    const VERSION: u32 = 1;
    type Attrs = TableCellAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .table_role(TableRole::Cell)
            .content("inline*")
            .marks(MarkPolicy::All)
            .defining()
            .isolating()
            .attr("alignment", serde_json::Value::Null)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TablesExtension;

impl RichTextExtension for TablesExtension {
    fn name(&self) -> &str {
        "tables"
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![
            TypedNodeSpec::of::<TableNode>(),
            TypedNodeSpec::of::<TableRowNode>(),
            TypedNodeSpec::of::<TableHeaderCellNode>(),
            TypedNodeSpec::of::<TableCellNode>(),
        ]
    }

    fn dom_views(&self) -> Vec<pine_richtext::render::NodeDomSpec> {
        dom::dom_specs()
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        vec![
            NodeSerializationSpec::for_node::<TableNode>()
                .markdown(MarkdownPolicy::Supported)
                .html(SemanticHtmlPolicy::dom(NodeDomSpec::nested::<TableNode>(
                    DomOutputSpec::element("table")
                        .child(DomOutputSpec::element("tbody").content_hole()),
                )))
                .plain_text(PlainTextPolicy::projected(
                    TextProjection::content_separated("\n"),
                ))
                .clipboard(ClipboardPolicy::Semantic),
            NodeSerializationSpec::for_node::<TableRowNode>()
                .markdown(MarkdownPolicy::Supported)
                .html(SemanticHtmlPolicy::dom(
                    NodeDomSpec::content::<TableRowNode>("tr"),
                ))
                .plain_text(PlainTextPolicy::projected(
                    TextProjection::content_separated("\t"),
                ))
                .clipboard(ClipboardPolicy::Semantic),
            NodeSerializationSpec::for_node::<TableHeaderCellNode>()
                .markdown(MarkdownPolicy::Supported)
                .html(SemanticHtmlPolicy::dom(
                    NodeDomSpec::content::<TableHeaderCellNode>("th").bind_attr(
                        DomAttrBinding::token(
                            "data-align",
                            "alignment",
                            ["left", "center", "right"],
                        ),
                    ),
                ))
                .plain_text(PlainTextPolicy::projected(TextProjection::content()))
                .clipboard(ClipboardPolicy::Semantic),
            NodeSerializationSpec::for_node::<TableCellNode>()
                .markdown(MarkdownPolicy::Supported)
                .html(SemanticHtmlPolicy::dom(
                    NodeDomSpec::content::<TableCellNode>("td").bind_attr(DomAttrBinding::token(
                        "data-align",
                        "alignment",
                        ["left", "center", "right"],
                    )),
                ))
                .plain_text(PlainTextPolicy::projected(TextProjection::content()))
                .clipboard(ClipboardPolicy::Semantic),
        ]
    }

    fn commands(&self) -> Vec<(String, NamedCommand)> {
        commands::named_commands()
    }

    fn key_bindings(&self) -> KeyBindings {
        commands::key_bindings()
    }

    fn markdown_node_emitters(&self) -> Vec<(String, pine_richtext::markdown::NodeEmitter)> {
        markdown::node_emitters()
    }

    fn markdown_parse_rules(&self) -> Vec<pine_richtext::markdown::MarkdownParseRule> {
        markdown::parse_rules()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::runtime::RuntimeBuilder;

    #[test]
    fn cells_are_isolating_edit_boundaries() {
        let runtime = RuntimeBuilder::new().with(TablesExtension).build();
        let schema = runtime.schema();

        assert!(
            schema
                .node_type(TableHeaderCellNode::NAME)
                .unwrap()
                .is_isolating()
        );
        assert!(
            schema
                .node_type(TableCellNode::NAME)
                .unwrap()
                .is_isolating()
        );
    }
}
