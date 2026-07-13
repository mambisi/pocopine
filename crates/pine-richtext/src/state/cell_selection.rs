use crate::model::{Fragment, Node, Schema, Slice, TableRole};
use crate::{RichTextError, RichTextResult};

use super::SelectionRange;

/// Grid rectangle selected inside one semantic table.
///
/// `right` and `bottom` are exclusive. `table_pos` is the absolute model
/// position immediately before the table node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSelectionRect {
    pub table_pos: usize,
    pub left: usize,
    pub top: usize,
    pub right: usize,
    pub bottom: usize,
}

impl CellSelectionRect {
    pub fn width(self) -> usize {
        self.right - self.left
    }

    pub fn height(self) -> usize {
        self.bottom - self.top
    }
}

#[derive(Clone, Debug)]
struct CellSlot {
    row: usize,
    column: usize,
    pos: usize,
    node: Node,
}

#[derive(Clone, Debug)]
pub(super) struct ResolvedCellSelection {
    table: Node,
    rect: CellSelectionRect,
    cells: Vec<CellSlot>,
}

impl ResolvedCellSelection {
    pub(super) fn rect(&self) -> CellSelectionRect {
        self.rect
    }

    pub(super) fn positions(&self) -> Vec<usize> {
        self.selected_cells().map(|cell| cell.pos).collect()
    }

    pub(super) fn ranges(&self) -> Vec<SelectionRange> {
        self.selected_cells()
            .map(|cell| SelectionRange {
                from: cell.pos + 1,
                to: cell.pos + cell.node.node_size() - 1,
            })
            .collect()
    }

    pub(super) fn slice(&self) -> RichTextResult<Slice> {
        let mut rows = Vec::with_capacity(self.rect.height());
        for row in self.rect.top..self.rect.bottom {
            let source_row = self.table.child(row).ok_or_else(|| {
                RichTextError::Selection(format!(
                    "selected table row {row} disappeared while creating a rectangular slice"
                ))
            })?;
            let cells = source_row.content().as_slice()[self.rect.left..self.rect.right].to_vec();
            rows.push(source_row.copy_with_content(Fragment::from(cells)));
        }
        let table = self.table.copy_with_content(Fragment::from(rows));
        Ok(Slice::new(Fragment::from(table), 0, 0))
    }

    pub(super) fn rectangular_replacements(
        &self,
        slice: &Slice,
        schema: &Schema,
    ) -> RichTextResult<Option<Vec<(SelectionRange, Fragment)>>> {
        if slice.open_start != 0 || slice.open_end != 0 || slice.content.len() != 1 {
            return Ok(None);
        }
        let Some(source_table) = slice.content.child(0) else {
            return Ok(None);
        };
        if schema
            .node_type(source_table.type_name())
            .ok()
            .and_then(|node_type| node_type.table_role())
            != Some(TableRole::Table)
        {
            return Ok(None);
        }
        let (source_width, source_cells) = project_table(schema, source_table, 0)?;
        let source_height = source_table.child_count();
        if source_width != self.rect.width() || source_height != self.rect.height() {
            return Err(RichTextError::Selection(format!(
                "cannot paste a {source_width}x{source_height} cell rectangle into a {}x{} selection",
                self.rect.width(),
                self.rect.height()
            )));
        }

        let replacements = self
            .selected_cells()
            .zip(source_cells)
            .map(|(target, source)| {
                (
                    SelectionRange {
                        from: target.pos + 1,
                        to: target.pos + target.node.node_size() - 1,
                    },
                    source.node.content().clone(),
                )
            })
            .collect();
        Ok(Some(replacements))
    }

    fn selected_cells(&self) -> impl Iterator<Item = &CellSlot> {
        self.cells.iter().filter(|cell| {
            (self.rect.top..self.rect.bottom).contains(&cell.row)
                && (self.rect.left..self.rect.right).contains(&cell.column)
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct Endpoint {
    table_pos: usize,
    row: usize,
    column: usize,
}

pub(super) fn resolve(
    doc: &Node,
    schema: &Schema,
    anchor_cell: usize,
    head_cell: usize,
) -> RichTextResult<ResolvedCellSelection> {
    let anchor = endpoint(doc, schema, anchor_cell, "anchor")?;
    let head = endpoint(doc, schema, head_cell, "head")?;
    if anchor.table_pos != head.table_pos {
        return Err(RichTextError::Selection(format!(
            "cell selection endpoints belong to different tables ({} and {})",
            anchor.table_pos, head.table_pos
        )));
    }

    let table = doc
        .node_at(anchor.table_pos)?
        .ok_or_else(|| RichTextError::Selection("selected table no longer exists".to_string()))?
        .clone();
    require_role(schema, &table, TableRole::Table, "selected table")?;
    if table.is_leaf() {
        return Err(RichTextError::Selection(
            "a semantic table cannot be a leaf node".to_string(),
        ));
    }

    let (width, cells) = project_table(schema, &table, anchor.table_pos)?;
    let height = table.child_count();
    if anchor.row >= height || head.row >= height || anchor.column >= width || head.column >= width
    {
        return Err(RichTextError::Selection(
            "cell selection endpoint is outside the table grid".to_string(),
        ));
    }
    let expected_anchor = &cells[anchor.row * width + anchor.column];
    let expected_head = &cells[head.row * width + head.column];
    if expected_anchor.pos != anchor_cell || expected_head.pos != head_cell {
        return Err(RichTextError::Selection(
            "cell selection endpoint is not positioned immediately before its semantic cell"
                .to_string(),
        ));
    }

    Ok(ResolvedCellSelection {
        table,
        rect: CellSelectionRect {
            table_pos: anchor.table_pos,
            left: anchor.column.min(head.column),
            top: anchor.row.min(head.row),
            right: anchor.column.max(head.column) + 1,
            bottom: anchor.row.max(head.row) + 1,
        },
        cells,
    })
}

pub(super) fn structural_bounds(
    doc: &Node,
    anchor_cell: usize,
    head_cell: usize,
) -> Option<(usize, usize)> {
    let anchor = structural_endpoint(doc, anchor_cell)?;
    let head = structural_endpoint(doc, head_cell)?;
    if anchor.table_pos != head.table_pos {
        return None;
    }
    let table = doc.node_at(anchor.table_pos).ok()??;
    let top = anchor.row.min(head.row);
    let bottom = anchor.row.max(head.row);
    let left = anchor.column.min(head.column);
    let right = anchor.column.max(head.column);
    let first = structural_cell_position(table, anchor.table_pos, top, left)?;
    let last = structural_cell_position(table, anchor.table_pos, bottom, right)?;
    Some((first.0, last.0 + last.1.node_size()))
}

fn structural_endpoint(doc: &Node, pos: usize) -> Option<Endpoint> {
    let resolved = doc.resolve(pos).ok()?;
    if resolved.text_offset() != 0 {
        return None;
    }
    let row_depth = resolved.depth();
    let table_depth = row_depth.checked_sub(1)?;
    Some(Endpoint {
        table_pos: resolved.before(table_depth)?,
        row: resolved.index(table_depth)?,
        column: resolved.index(row_depth)?,
    })
}

fn structural_cell_position(
    table: &Node,
    table_pos: usize,
    target_row: usize,
    target_column: usize,
) -> Option<(usize, &Node)> {
    let mut row_pos = table_pos + 1;
    for (row_index, row) in table.content().iter().enumerate() {
        if row_index == target_row {
            let mut cell_pos = row_pos + 1;
            for (column, cell) in row.content().iter().enumerate() {
                if column == target_column {
                    return Some((cell_pos, cell));
                }
                cell_pos += cell.node_size();
            }
            return None;
        }
        row_pos += row.node_size();
    }
    None
}

fn endpoint(doc: &Node, schema: &Schema, pos: usize, label: &str) -> RichTextResult<Endpoint> {
    let resolved = doc.resolve(pos).map_err(|_| {
        RichTextError::Selection(format!(
            "cell selection {label} position {pos} is outside the document"
        ))
    })?;
    if resolved.text_offset() != 0 {
        return Err(RichTextError::Selection(format!(
            "cell selection {label} position {pos} is inside text, not before a cell"
        )));
    }
    let row_depth = resolved.depth();
    let table_depth = row_depth.checked_sub(1).ok_or_else(|| {
        RichTextError::Selection(format!(
            "cell selection {label} position {pos} has no enclosing semantic table"
        ))
    })?;
    let row = resolved.parent();
    require_role(schema, row, TableRole::Row, "cell parent")?;
    let table = resolved.node(table_depth).ok_or_else(|| {
        RichTextError::Selection(format!(
            "cell selection {label} position {pos} has no enclosing semantic table"
        ))
    })?;
    require_role(schema, table, TableRole::Table, "cell grandparent")?;
    let cell = resolved.node_after().ok_or_else(|| {
        RichTextError::Selection(format!(
            "cell selection {label} position {pos} does not point before a node"
        ))
    })?;
    require_role(schema, &cell, TableRole::Cell, "selection endpoint")?;
    if cell.is_leaf() {
        return Err(RichTextError::Selection(format!(
            "cell selection {label} position {pos} points at a leaf cell"
        )));
    }

    let table_pos = resolved.before(table_depth).ok_or_else(|| {
        RichTextError::Selection(format!(
            "cell selection {label} position {pos} cannot locate its table boundary"
        ))
    })?;
    Ok(Endpoint {
        table_pos,
        row: resolved.index(table_depth).ok_or_else(|| {
            RichTextError::Selection("cannot resolve selected table row index".to_string())
        })?,
        column: resolved.index(row_depth).ok_or_else(|| {
            RichTextError::Selection("cannot resolve selected table column index".to_string())
        })?,
    })
}

fn project_table(
    schema: &Schema,
    table: &Node,
    table_pos: usize,
) -> RichTextResult<(usize, Vec<CellSlot>)> {
    if table.child_count() == 0 {
        return Err(RichTextError::Selection(
            "semantic table has no rows".to_string(),
        ));
    }

    let mut width = None;
    let mut cells = Vec::new();
    let mut row_pos = table_pos + 1;
    for (row_index, row) in table.content().iter().enumerate() {
        require_role(schema, row, TableRole::Row, "table child")?;
        if row.is_leaf() || row.child_count() == 0 {
            return Err(RichTextError::Selection(format!(
                "semantic table row {row_index} has no cells"
            )));
        }
        let expected_width = *width.get_or_insert(row.child_count());
        if row.child_count() != expected_width {
            return Err(RichTextError::Selection(format!(
                "semantic table is ragged: row {row_index} has {} cells, expected {expected_width}",
                row.child_count()
            )));
        }

        let mut cell_pos = row_pos + 1;
        for (column, cell) in row.content().iter().enumerate() {
            require_role(schema, cell, TableRole::Cell, "table row child")?;
            if cell.is_leaf() {
                return Err(RichTextError::Selection(format!(
                    "semantic table cell {row_index},{column} is a leaf node"
                )));
            }
            cells.push(CellSlot {
                row: row_index,
                column,
                pos: cell_pos,
                node: cell.clone(),
            });
            cell_pos += cell.node_size();
        }
        row_pos += row.node_size();
    }

    Ok((width.expect("a non-empty table has a first row"), cells))
}

fn require_role(
    schema: &Schema,
    node: &Node,
    expected: TableRole,
    context: &str,
) -> RichTextResult<()> {
    let node_type = schema.node_type(node.type_name()).map_err(|_| {
        RichTextError::Selection(format!(
            "{context} has unknown node type `{}`",
            node.type_name()
        ))
    })?;
    if node_type.table_role() == Some(expected) {
        Ok(())
    } else {
        Err(RichTextError::Selection(format!(
            "{context} node `{}` has table role {:?}, expected {:?}",
            node.type_name(),
            node_type.table_role(),
            expected
        )))
    }
}
