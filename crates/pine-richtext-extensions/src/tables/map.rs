use std::error::Error;
use std::fmt;

use pine_richtext::model::Node;

use super::{
    MAX_COLUMN_WIDTH, MAX_ROW_HEIGHT, MIN_COLUMN_WIDTH, MIN_ROW_HEIGHT, TableAttrs, TableRowAttrs,
};

/// Absolute rectangle occupied by one semantic table cell.
///
/// V1 tables do not support spans, so every cell is exactly one grid slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRect {
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
    /// Absolute document position immediately before the cell node.
    pub pos: usize,
}

#[derive(Clone, Copy, Debug)]
struct CellEntry {
    rect: CellRect,
    node_size: usize,
}

/// Validated rectangular projection of one semantic table.
#[derive(Clone, Debug)]
pub struct TableMap {
    table_pos: usize,
    width: usize,
    height: usize,
    row_positions: Vec<usize>,
    cells: Vec<CellEntry>,
}

impl TableMap {
    /// Validate `table` and build its absolute row-major position map.
    pub fn new(table: &Node, table_pos: usize) -> Result<Self, TableMapError> {
        if table.type_name() != "table" {
            return Err(TableMapError::NotTable {
                found: table.type_name().to_string(),
            });
        }
        if table.child_count() == 0 {
            return Err(TableMapError::EmptyTable);
        }

        let table_attrs = decode_attrs::<TableAttrs>(table, "table")?;
        let mut width = None;
        let mut row_positions = Vec::with_capacity(table.child_count());
        let mut cells = Vec::new();
        let mut row_pos = table_pos + 1;

        for (row_index, row) in table.content().iter().enumerate() {
            if row.type_name() != "table_row" {
                return Err(TableMapError::InvalidRowType {
                    row: row_index,
                    found: row.type_name().to_string(),
                });
            }
            if row.child_count() == 0 {
                return Err(TableMapError::EmptyRow { row: row_index });
            }
            let expected = *width.get_or_insert(row.child_count());
            if row.child_count() != expected {
                return Err(TableMapError::RaggedRow {
                    row: row_index,
                    expected,
                    found: row.child_count(),
                });
            }

            let row_attrs = decode_attrs::<TableRowAttrs>(row, "table_row")?;
            if let Some(height) = row_attrs.height
                && !(MIN_ROW_HEIGHT..=MAX_ROW_HEIGHT).contains(&height)
            {
                return Err(TableMapError::InvalidRowHeight {
                    row: row_index,
                    height,
                });
            }

            row_positions.push(row_pos);
            let mut cell_pos = row_pos + 1;
            for (column, cell) in row.content().iter().enumerate() {
                let expected_type = if row_index == 0 {
                    "table_header_cell"
                } else {
                    "table_cell"
                };
                if cell.type_name() != expected_type {
                    return Err(TableMapError::InvalidCellType {
                        row: row_index,
                        column,
                        expected: expected_type,
                        found: cell.type_name().to_string(),
                    });
                }
                cells.push(CellEntry {
                    rect: CellRect {
                        left: column,
                        top: row_index,
                        right: column + 1,
                        bottom: row_index + 1,
                        pos: cell_pos,
                    },
                    node_size: cell.node_size(),
                });
                cell_pos += cell.node_size();
            }
            row_pos += row.node_size();
        }

        let width = width.expect("a non-empty table has a first row");
        if table_attrs.column_widths.len() > width {
            return Err(TableMapError::TooManyColumnWidths {
                columns: width,
                widths: table_attrs.column_widths.len(),
            });
        }
        for (column, width) in table_attrs.column_widths.iter().enumerate() {
            if let Some(width) = width
                && !(MIN_COLUMN_WIDTH..=MAX_COLUMN_WIDTH).contains(width)
            {
                return Err(TableMapError::InvalidColumnWidth {
                    column,
                    width: *width,
                });
            }
        }

        Ok(Self {
            table_pos,
            width,
            height: table.child_count(),
            row_positions,
            cells,
        })
    }

    pub fn table_pos(&self) -> usize {
        self.table_pos
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn row_position(&self, row: usize) -> Option<usize> {
        self.row_positions.get(row).copied()
    }

    pub fn cell(&self, row: usize, column: usize) -> Option<CellRect> {
        if row >= self.height || column >= self.width {
            return None;
        }
        Some(self.cells[row * self.width + column].rect)
    }

    /// Find the cell whose node/content range contains `pos`.
    pub fn find_cell(&self, pos: usize) -> Option<CellRect> {
        self.cells
            .iter()
            .find(|entry| entry.rect.pos == pos)
            .or_else(|| {
                self.cells
                    .iter()
                    .find(|entry| pos > entry.rect.pos && pos < entry.rect.pos + entry.node_size)
            })
            .map(|entry| entry.rect)
    }

    /// Minimal grid rectangle containing the cells at `anchor` and `head`.
    pub fn rect_between(&self, anchor: usize, head: usize) -> Option<CellRect> {
        let anchor = self.find_cell(anchor)?;
        let head = self.find_cell(head)?;
        Some(CellRect {
            left: anchor.left.min(head.left),
            top: anchor.top.min(head.top),
            right: anchor.right.max(head.right),
            bottom: anchor.bottom.max(head.bottom),
            pos: anchor.pos,
        })
    }

    /// Absolute positions of cells whose slots fall inside `rect`, row-major.
    pub fn cells_in_rect(&self, rect: CellRect) -> Option<Vec<usize>> {
        if rect.left >= rect.right
            || rect.top >= rect.bottom
            || rect.right > self.width
            || rect.bottom > self.height
        {
            return None;
        }
        let mut positions =
            Vec::with_capacity((rect.right - rect.left).saturating_mul(rect.bottom - rect.top));
        for row in rect.top..rect.bottom {
            for column in rect.left..rect.right {
                positions.push(self.cell(row, column)?.pos);
            }
        }
        Some(positions)
    }
}

fn decode_attrs<A>(node: &Node, node_type: &str) -> Result<A, TableMapError>
where
    A: serde::de::DeserializeOwned,
{
    let value =
        serde_json::to_value(node.attrs()).map_err(|error| TableMapError::InvalidAttrs {
            node_type: node_type.to_string(),
            error: error.to_string(),
        })?;
    serde_json::from_value(value).map_err(|error| TableMapError::InvalidAttrs {
        node_type: node_type.to_string(),
        error: error.to_string(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableMapError {
    NotTable {
        found: String,
    },
    EmptyTable,
    InvalidRowType {
        row: usize,
        found: String,
    },
    EmptyRow {
        row: usize,
    },
    RaggedRow {
        row: usize,
        expected: usize,
        found: usize,
    },
    InvalidCellType {
        row: usize,
        column: usize,
        expected: &'static str,
        found: String,
    },
    TooManyColumnWidths {
        columns: usize,
        widths: usize,
    },
    InvalidColumnWidth {
        column: usize,
        width: u32,
    },
    InvalidRowHeight {
        row: usize,
        height: u32,
    },
    InvalidAttrs {
        node_type: String,
        error: String,
    },
}

impl fmt::Display for TableMapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTable { found } => write!(formatter, "expected table node, found `{found}`"),
            Self::EmptyTable => formatter.write_str("table must contain at least one row"),
            Self::InvalidRowType { row, found } => {
                write!(formatter, "table row {row} has node type `{found}`")
            }
            Self::EmptyRow { row } => write!(formatter, "table row {row} has no cells"),
            Self::RaggedRow {
                row,
                expected,
                found,
            } => write!(
                formatter,
                "table row {row} has {found} cells; expected {expected}"
            ),
            Self::InvalidCellType {
                row,
                column,
                expected,
                found,
            } => write!(
                formatter,
                "table cell {row},{column} has type `{found}`; expected `{expected}`"
            ),
            Self::TooManyColumnWidths { columns, widths } => write!(
                formatter,
                "table has {columns} columns but {widths} persisted column widths"
            ),
            Self::InvalidColumnWidth { column, width } => write!(
                formatter,
                "table column {column} width {width}px is outside {MIN_COLUMN_WIDTH}..={MAX_COLUMN_WIDTH}"
            ),
            Self::InvalidRowHeight { row, height } => write!(
                formatter,
                "table row {row} height {height}px is outside {MIN_ROW_HEIGHT}..={MAX_ROW_HEIGHT}"
            ),
            Self::InvalidAttrs { node_type, error } => {
                write!(formatter, "invalid `{node_type}` attrs: {error}")
            }
        }
    }
}

impl Error for TableMapError {}
