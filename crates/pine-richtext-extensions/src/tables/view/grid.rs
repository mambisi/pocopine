//! Table-view projection over an immutable semantic child fragment.

use std::fmt;

use pine_richtext::model::Fragment;

use super::controller::{CellSelectionCommit, TableViewAnchor};

/// One logical cell and its absolute model position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewCell {
    pub row: usize,
    pub column: usize,
    pub pos: usize,
}

/// Rectangular projection used by component chrome and DOM painting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableViewGrid {
    anchor: TableViewAnchor,
    width: usize,
    height: usize,
    cells: Vec<ViewCell>,
}

impl TableViewGrid {
    pub fn from_content(
        content: &Fragment,
        anchor: TableViewAnchor,
    ) -> Result<Self, TableViewGridError> {
        if content.is_empty() {
            return Err(TableViewGridError::Empty);
        }
        let mut width = None;
        let mut cells = Vec::new();
        let mut row_pos = anchor.table_pos + 1;
        for (row_index, row) in content.iter().enumerate() {
            if row.type_name() != "table_row" {
                return Err(TableViewGridError::WrongRow {
                    row: row_index,
                    actual: row.type_name().to_string(),
                });
            }
            if row.child_count() == 0 {
                return Err(TableViewGridError::EmptyRow { row: row_index });
            }
            let expected = *width.get_or_insert(row.child_count());
            if row.child_count() != expected {
                return Err(TableViewGridError::Ragged {
                    row: row_index,
                    expected,
                    actual: row.child_count(),
                });
            }
            let mut cell_pos = row_pos + 1;
            for (column, cell) in row.content().iter().enumerate() {
                let expected_type = if row_index == 0 {
                    "table_header_cell"
                } else {
                    "table_cell"
                };
                if cell.type_name() != expected_type {
                    return Err(TableViewGridError::WrongCell {
                        row: row_index,
                        column,
                        expected: expected_type,
                        actual: cell.type_name().to_string(),
                    });
                }
                cells.push(ViewCell {
                    row: row_index,
                    column,
                    pos: cell_pos,
                });
                cell_pos += cell.node_size();
            }
            row_pos += row.node_size();
        }
        Ok(Self {
            anchor,
            width: width.expect("non-empty table has a first row"),
            height: content.len(),
            cells,
        })
    }

    pub fn anchor(&self) -> TableViewAnchor {
        self.anchor
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn cell(&self, row: usize, column: usize) -> Option<ViewCell> {
        if row >= self.height || column >= self.width {
            return None;
        }
        self.cells.get(row * self.width + column).copied()
    }

    pub fn coordinate_at(&self, position: usize) -> Option<ViewCell> {
        self.cells.iter().copied().find(|cell| cell.pos == position)
    }

    pub fn select_cell(&self, row: usize, column: usize) -> Option<CellSelectionCommit> {
        let cell = self.cell(row, column)?;
        Some(self.selection(cell, cell))
    }

    pub fn select_row(&self, row: usize) -> Option<CellSelectionCommit> {
        Some(self.selection(self.cell(row, 0)?, self.cell(row, self.width - 1)?))
    }

    pub fn select_column(&self, column: usize) -> Option<CellSelectionCommit> {
        Some(self.selection(self.cell(0, column)?, self.cell(self.height - 1, column)?))
    }

    pub fn select_table(&self) -> CellSelectionCommit {
        self.selection(
            self.cells[0],
            *self.cells.last().expect("non-empty table has cells"),
        )
    }

    pub fn selection_rect(
        &self,
        anchor_cell: usize,
        head_cell: usize,
    ) -> Option<ViewSelectionRect> {
        let anchor = self.coordinate_at(anchor_cell)?;
        let head = self.coordinate_at(head_cell)?;
        Some(ViewSelectionRect {
            left: anchor.column.min(head.column),
            top: anchor.row.min(head.row),
            right: anchor.column.max(head.column) + 1,
            bottom: anchor.row.max(head.row) + 1,
        })
    }

    fn selection(&self, anchor: ViewCell, head: ViewCell) -> CellSelectionCommit {
        CellSelectionCommit {
            anchor: self.anchor,
            anchor_row: anchor.row,
            anchor_column: anchor.column,
            head_row: head.row,
            head_column: head.column,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewSelectionRect {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl ViewSelectionRect {
    pub fn contains(self, row: usize, column: usize) -> bool {
        row >= self.top && row < self.bottom && column >= self.left && column < self.right
    }

    pub fn covers_row(self, row: usize, table_width: usize) -> bool {
        row >= self.top && row < self.bottom && self.left == 0 && self.right == table_width
    }

    pub fn covers_column(self, column: usize, table_height: usize) -> bool {
        column >= self.left && column < self.right && self.top == 0 && self.bottom == table_height
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableViewGridError {
    Empty,
    WrongRow {
        row: usize,
        actual: String,
    },
    EmptyRow {
        row: usize,
    },
    Ragged {
        row: usize,
        expected: usize,
        actual: usize,
    },
    WrongCell {
        row: usize,
        column: usize,
        expected: &'static str,
        actual: String,
    },
}

impl fmt::Display for TableViewGridError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("table view content has no rows"),
            Self::WrongRow { row, actual } => {
                write!(formatter, "table row {row} has node type `{actual}`")
            }
            Self::EmptyRow { row } => write!(formatter, "table row {row} has no cells"),
            Self::Ragged {
                row,
                expected,
                actual,
            } => write!(
                formatter,
                "table row {row} has {actual} cells, expected {expected}"
            ),
            Self::WrongCell {
                row,
                column,
                expected,
                actual,
            } => write!(
                formatter,
                "table cell {row},{column} has node type `{actual}`, expected `{expected}`"
            ),
        }
    }
}

impl std::error::Error for TableViewGridError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::{TableCellAttrs, TableHeaderCellAttrs, TableRowAttrs};
    use pine_richtext::model::Fragment;
    use pine_richtext::runtime::RuntimeBuilder;
    use serde::Serialize;

    fn attrs(value: &impl Serialize) -> pine_richtext::model::Attrs {
        serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap()
    }

    fn grid() -> TableViewGrid {
        let runtime = RuntimeBuilder::new()
            .with(crate::tables::TablesExtension)
            .build();
        let schema = runtime.schema();
        let header = (0..3)
            .map(|_| {
                schema
                    .node(
                        "table_header_cell",
                        attrs(&TableHeaderCellAttrs::default()),
                        Fragment::empty(),
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let body = (0..3)
            .map(|_| {
                schema
                    .node(
                        "table_cell",
                        attrs(&TableCellAttrs::default()),
                        Fragment::empty(),
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        let rows = vec![header, body]
            .into_iter()
            .map(|cells| {
                schema
                    .node(
                        "table_row",
                        attrs(&TableRowAttrs::default()),
                        Fragment::from(cells),
                    )
                    .unwrap()
            })
            .collect::<Vec<_>>();
        TableViewGrid::from_content(&Fragment::from(rows), TableViewAnchor { table_pos: 10 })
            .unwrap()
    }

    #[test]
    fn row_column_and_table_selection_are_rectangular() {
        let grid = grid();
        let column = grid.select_column(1).unwrap();
        let rect = grid
            .selection_rect(
                grid.cell(column.anchor_row, column.anchor_column)
                    .unwrap()
                    .pos,
                grid.cell(column.head_row, column.head_column).unwrap().pos,
            )
            .unwrap();
        assert_eq!(
            rect,
            ViewSelectionRect {
                left: 1,
                top: 0,
                right: 2,
                bottom: 2,
            }
        );
        assert!(rect.covers_column(1, grid.height()));
        assert!(!rect.covers_row(0, grid.width()));

        let table = grid.select_table();
        let rect = grid
            .selection_rect(
                grid.cell(table.anchor_row, table.anchor_column)
                    .unwrap()
                    .pos,
                grid.cell(table.head_row, table.head_column).unwrap().pos,
            )
            .unwrap();
        assert!(rect.covers_row(0, grid.width()));
        assert!(rect.covers_column(2, grid.height()));
    }
}
