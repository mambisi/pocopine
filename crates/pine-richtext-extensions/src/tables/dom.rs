//! Extension-owned native DOM specs for semantic table rows and cells.
//!
//! The runtime compiles these typed structures to tags and recursively fills
//! their content holes. No extension code assembles HTML strings.

use pine_richtext::render::{DomAttrBinding, DomOutputSpec, NodeDomSpec};

use super::{TableCellNode, TableHeaderCellNode, TableNode, TableRowNode};

pub const TABLE_ROW_CLASS: &str = "pine-richtext-table-row";
pub const TABLE_CELL_CLASS: &str = "pine-richtext-table-cell";
pub const TABLE_HEADER_CELL_CLASS: &str = "pine-richtext-table-header-cell";
pub const TABLE_ROW_ATTR: &str = "data-pine-table-row";
pub const TABLE_CELL_ATTR: &str = "data-pine-table-cell";
pub const TABLE_SELECTED_ATTR: &str = "data-selected";

pub(super) fn dom_specs() -> Vec<NodeDomSpec> {
    vec![
        NodeDomSpec::nested::<TableNode>(
            DomOutputSpec::element("table")
                .class("pine-richtext-table")
                .attr("role", "grid")
                .child(
                    DomOutputSpec::element("tbody")
                        .class("pine-richtext-table-body")
                        .attr("role", "rowgroup")
                        .content_hole(),
                ),
        ),
        NodeDomSpec::content::<TableRowNode>("tr")
            .class(TABLE_ROW_CLASS)
            .attr(TABLE_ROW_ATTR, "true")
            .attr(TABLE_SELECTED_ATTR, "false")
            .attr("role", "row")
            .bind_attr(DomAttrBinding::integer("data-height", "height")),
        NodeDomSpec::content::<TableHeaderCellNode>("th")
            .class(format!("{TABLE_CELL_CLASS} {TABLE_HEADER_CELL_CLASS}"))
            .attr(TABLE_CELL_ATTR, "true")
            .attr(TABLE_SELECTED_ATTR, "false")
            .attr("aria-selected", "false")
            .attr("role", "columnheader")
            .bind_attr(DomAttrBinding::token(
                "data-align",
                "alignment",
                ["left", "center", "right"],
            )),
        NodeDomSpec::content::<TableCellNode>("td")
            .class(TABLE_CELL_CLASS)
            .attr(TABLE_CELL_ATTR, "true")
            .attr(TABLE_SELECTED_ATTR, "false")
            .attr("aria-selected", "false")
            .attr("role", "gridcell")
            .bind_attr(DomAttrBinding::token(
                "data-align",
                "alignment",
                ["left", "center", "right"],
            )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_types_compile_to_valid_table_tags() {
        let views = dom_specs();
        assert_eq!(views[0].root_tag(), "table");
        assert_eq!(views[1].root_tag(), "tr");
        assert_eq!(views[2].root_tag(), "th");
        assert_eq!(views[3].root_tag(), "td");
    }
}
