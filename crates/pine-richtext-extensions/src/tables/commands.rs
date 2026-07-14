use std::sync::Arc;

use pine_richtext::commands::BoxedCommand;
use pine_richtext::extension::{KeyBindingFactory, KeyBindings, NamedCommand};
use pine_richtext::model::{Attrs, Fragment, Node, Schema};
use pine_richtext::state::{EditorState, Selection, Transaction};
use serde::Serialize;
use serde_json::Value;

use super::{
    MAX_COLUMN_WIDTH, MAX_ROW_HEIGHT, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, MIN_COLUMN_WIDTH,
    MIN_ROW_HEIGHT, TableAlignment, TableAttrs, TableCellAttrs, TableHeaderCellAttrs, TableMap,
    TableRowAttrs,
};

#[cfg(feature = "table-view")]
use pine_richtext::view::{NodeCommand, NodeCommandTarget, NodeViewError};

#[cfg(feature = "table-view")]
use super::TableNode;

/// Anchored column resize used by the component table view.
#[cfg(feature = "table-view")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeColumn {
    pub expected_table_pos: usize,
    pub column: usize,
    pub width: u32,
}

#[cfg(feature = "table-view")]
impl NodeCommand<TableNode> for ResizeColumn {
    fn apply(
        self,
        state: &EditorState,
        target: NodeCommandTarget<TableNode>,
    ) -> Result<Option<Transaction>, NodeViewError> {
        require_target_position(self.expected_table_pos, target.position)?;
        let map = TableMap::new(&target.node, target.position).map_err(table_command_error)?;
        if self.column >= map.width() {
            return Err(table_command_error(format!(
                "column {} is outside a {}-column table",
                self.column,
                map.width()
            )));
        }
        let width = self.width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH);
        let mut attrs = target.attrs;
        attrs.column_widths.resize(map.width(), None);
        if attrs.column_widths[self.column] == Some(width) {
            return Ok(None);
        }
        attrs.column_widths[self.column] = Some(width);
        let table = rebuild_table(
            state.schema(),
            &target.node,
            target.node.content().iter().cloned().collect(),
            Some(attrs),
        )
        .ok_or_else(|| table_command_error("could not rebuild resized table"))?;
        replace_anchored_table(state, target.position, &target.node, table).map(Some)
    }
}

/// Anchored row resize used by the component table view.
#[cfg(feature = "table-view")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeRow {
    pub expected_table_pos: usize,
    pub row: usize,
    pub height: u32,
}

#[cfg(feature = "table-view")]
impl NodeCommand<TableNode> for ResizeRow {
    fn apply(
        self,
        state: &EditorState,
        target: NodeCommandTarget<TableNode>,
    ) -> Result<Option<Transaction>, NodeViewError> {
        require_target_position(self.expected_table_pos, target.position)?;
        let map = TableMap::new(&target.node, target.position).map_err(table_command_error)?;
        if self.row >= map.height() {
            return Err(table_command_error(format!(
                "row {} is outside a {}-row table",
                self.row,
                map.height()
            )));
        }
        let height = self.height.clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT);
        let mut rows = target.node.content().iter().cloned().collect::<Vec<_>>();
        let source = &rows[self.row];
        let attrs = row_attrs(source)
            .ok_or_else(|| table_command_error("could not decode table row attrs"))?;
        if attrs.height == Some(height) {
            return Ok(None);
        }
        rows[self.row] = state
            .schema()
            .node(
                "table_row",
                attrs_map(&TableRowAttrs {
                    height: Some(height),
                })
                .ok_or_else(|| table_command_error("could not encode table row attrs"))?,
                source.content().clone(),
            )
            .map_err(table_command_error)?;
        let table = rebuild_table(state.schema(), &target.node, rows, None)
            .ok_or_else(|| table_command_error("could not rebuild resized table"))?;
        replace_anchored_table(state, target.position, &target.node, table).map(Some)
    }
}

/// Anchored body-row reorder used by the component table view.
///
/// Row zero is the canonical header and cannot be moved or used as a target.
/// `target` is the row's final index after the move.
#[cfg(feature = "table-view")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveRow {
    pub expected_table_pos: usize,
    pub source: usize,
    pub target: usize,
}

#[cfg(feature = "table-view")]
impl NodeCommand<TableNode> for MoveRow {
    fn apply(
        self,
        state: &EditorState,
        target: NodeCommandTarget<TableNode>,
    ) -> Result<Option<Transaction>, NodeViewError> {
        require_target_position(self.expected_table_pos, target.position)?;
        let map = TableMap::new(&target.node, target.position).map_err(table_command_error)?;
        if !valid_row_move(map.height(), self.source, self.target) {
            return Ok(None);
        }
        let table = moved_row_table(
            state.schema(),
            &target.node,
            map.height(),
            self.source,
            self.target,
        )
        .ok_or_else(|| table_command_error("could not rebuild reordered table rows"))?;
        replace_anchored_table_with_cell_selection(
            state,
            target.position,
            &target.node,
            table,
            (self.target, 0),
            (self.target, map.width() - 1),
        )
        .map(Some)
    }
}

/// Anchored column reorder used by the component table view.
///
/// `target` is the column's final index after the move. Every row and the
/// table's persisted column-width metadata move together.
#[cfg(feature = "table-view")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveColumn {
    pub expected_table_pos: usize,
    pub source: usize,
    pub target: usize,
}

#[cfg(feature = "table-view")]
impl NodeCommand<TableNode> for MoveColumn {
    fn apply(
        self,
        state: &EditorState,
        target: NodeCommandTarget<TableNode>,
    ) -> Result<Option<Transaction>, NodeViewError> {
        require_target_position(self.expected_table_pos, target.position)?;
        let map = TableMap::new(&target.node, target.position).map_err(table_command_error)?;
        if !valid_column_move(map.width(), self.source, self.target) {
            return Ok(None);
        }
        let table = moved_column_table(
            state.schema(),
            &target.node,
            map.width(),
            target.attrs,
            self.source,
            self.target,
        )
        .ok_or_else(|| table_command_error("could not rebuild reordered table columns"))?;
        replace_anchored_table_with_cell_selection(
            state,
            target.position,
            &target.node,
            table,
            (0, self.target),
            (map.height() - 1, self.target),
        )
        .map(Some)
    }
}

/// Anchored rectangular semantic selection used by cell/row/column/table
/// selector chrome.
#[cfg(feature = "table-view")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectCells {
    pub expected_table_pos: usize,
    pub anchor_row: usize,
    pub anchor_column: usize,
    pub head_row: usize,
    pub head_column: usize,
}

#[cfg(feature = "table-view")]
impl NodeCommand<TableNode> for SelectCells {
    fn apply(
        self,
        state: &EditorState,
        target: NodeCommandTarget<TableNode>,
    ) -> Result<Option<Transaction>, NodeViewError> {
        require_target_position(self.expected_table_pos, target.position)?;
        let map = TableMap::new(&target.node, target.position).map_err(table_command_error)?;
        let anchor = map
            .cell(self.anchor_row, self.anchor_column)
            .ok_or_else(|| table_command_error("cell-selection anchor is outside the table"))?;
        let head = map
            .cell(self.head_row, self.head_column)
            .ok_or_else(|| table_command_error("cell-selection head is outside the table"))?;
        let mut transaction = state.tr();
        transaction
            .set_selection(Selection::cells(anchor.pos, head.pos))
            .map_err(table_command_error)?;
        Ok(Some(transaction))
    }
}

#[cfg(feature = "table-view")]
fn replace_anchored_table(
    state: &EditorState,
    position: usize,
    source: &Node,
    table: Node,
) -> Result<Transaction, NodeViewError> {
    let mut transaction = state.tr();
    transaction
        .replace_with(
            position,
            position.saturating_add(source.node_size()),
            Fragment::from(table),
        )
        .map_err(table_command_error)?;
    Ok(transaction)
}

#[cfg(feature = "table-view")]
fn replace_anchored_table_with_cell_selection(
    state: &EditorState,
    position: usize,
    source: &Node,
    table: Node,
    anchor: (usize, usize),
    head: (usize, usize),
) -> Result<Transaction, NodeViewError> {
    let map = TableMap::new(&table, position).map_err(table_command_error)?;
    let mut transaction = replace_anchored_table(state, position, source, table)?;
    let anchor = map.cell(anchor.0, anchor.1).ok_or_else(|| {
        table_command_error("reordered table selection anchor is outside the table")
    })?;
    let head = map.cell(head.0, head.1).ok_or_else(|| {
        table_command_error("reordered table selection head is outside the table")
    })?;
    transaction
        .set_selection(Selection::cells(anchor.pos, head.pos))
        .map_err(table_command_error)?;
    Ok(transaction)
}

#[cfg(feature = "table-view")]
fn require_target_position(expected: usize, actual: usize) -> Result<(), NodeViewError> {
    if expected == actual {
        Ok(())
    } else {
        Err(table_command_error(format!(
            "table moved from position {expected} to {actual} during the interaction"
        )))
    }
}

#[cfg(feature = "table-view")]
fn table_command_error(error: impl std::fmt::Display) -> NodeViewError {
    NodeViewError::Dispatch {
        node_type: "table".to_string(),
        message: error.to_string(),
    }
}

pub fn insert_table(rows: usize, columns: usize) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        if rows == 0 || columns == 0 || rows > MAX_TABLE_ROWS || columns > MAX_TABLE_COLUMNS {
            return None;
        }
        let table = build_empty_table(state.schema(), rows, columns)?;
        let insertion_hint = state.selection().from(state.doc());
        let mut transaction = state.tr();
        transaction.replace_selection_with(table).ok()?;
        let table_pos = nearest_table_position(transaction.doc(), insertion_hint)?;
        let table = transaction.doc().node_at(table_pos).ok()??;
        let map = TableMap::new(table, table_pos).ok()?;
        transaction
            .set_selection(Selection::text(map.cell(0, 0)?.pos + 1))
            .ok()?;
        Some(transaction)
    })
}

pub fn delete_table() -> BoxedCommand {
    Box::new(|state: &EditorState| {
        let located = locate_table(state)?;
        let mut transaction = state.tr();
        transaction
            .delete(
                located.map.table_pos(),
                located.map.table_pos() + located.table.node_size(),
            )
            .ok()?;
        Some(transaction)
    })
}

pub fn insert_row_before() -> BoxedCommand {
    insert_row(RowSide::Before)
}

pub fn insert_row_after() -> BoxedCommand {
    insert_row(RowSide::After)
}

fn insert_row(side: RowSide) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let located = locate_table(state)?;
        if located.map.height() >= MAX_TABLE_ROWS {
            return None;
        }
        let current = located.cell.map(|cell| cell.top).unwrap_or(0);
        let index = match side {
            RowSide::Before => current,
            RowSide::After => current + 1,
        };
        let mut rows = located.table.content().iter().cloned().collect::<Vec<_>>();
        let alignments = column_alignments(located.table, located.map.width());
        let header = index == 0;
        let new_row = build_empty_row(state.schema(), header, &alignments)?;
        if header {
            rows[0] = retype_row(state.schema(), &rows[0], false)?;
        }
        rows.insert(index, new_row);
        let table = rebuild_table(state.schema(), located.table, rows, None)?;
        replace_table(state, &located, table, Some((index, 0)))
    })
}

pub fn delete_row() -> BoxedCommand {
    Box::new(|state: &EditorState| {
        let located = locate_table(state)?;
        let row = located.cell.map(|cell| cell.top).unwrap_or(0);
        if located.map.height() == 1 {
            return delete_table().apply(state);
        }
        let mut rows = located.table.content().iter().cloned().collect::<Vec<_>>();
        rows.remove(row);
        if row == 0 {
            rows[0] = retype_row(state.schema(), &rows[0], true)?;
        }
        let target_row = row.min(rows.len() - 1);
        let table = rebuild_table(state.schema(), located.table, rows, None)?;
        replace_table(state, &located, table, Some((target_row, 0)))
    })
}

/// Move a body row to a final body-row index.
///
/// Row zero is the canonical header and remains pinned. Moving from or to row
/// zero, using an out-of-range index, or moving a row to itself is a no-op. On
/// success, the selection lands in the moved row at its previous column (or
/// column zero when the table was not selected).
pub fn move_row(source: usize, target: usize) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let located = locate_table(state)?;
        if !valid_row_move(located.map.height(), source, target) {
            return None;
        }
        let selected_column = located.cell.map(|cell| cell.left).unwrap_or(0);
        let table = moved_row_table(
            state.schema(),
            located.table,
            located.map.height(),
            source,
            target,
        )?;
        replace_table(state, &located, table, Some((target, selected_column)))
    })
}

pub fn insert_column_before() -> BoxedCommand {
    insert_column(ColumnSide::Before)
}

pub fn insert_column_after() -> BoxedCommand {
    insert_column(ColumnSide::After)
}

fn insert_column(side: ColumnSide) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let located = locate_table(state)?;
        if located.map.width() >= MAX_TABLE_COLUMNS {
            return None;
        }
        let current = located.cell.map(|cell| cell.left).unwrap_or(0);
        let column = match side {
            ColumnSide::Before => current,
            ColumnSide::After => current + 1,
        };
        let mut rows = Vec::with_capacity(located.map.height());
        for (row_index, row) in located.table.content().iter().enumerate() {
            let mut cells = row.content().iter().cloned().collect::<Vec<_>>();
            let alignment = nearest_alignment(&cells, column);
            cells.insert(
                column,
                build_empty_cell(state.schema(), row_index == 0, alignment)?,
            );
            rows.push(rebuild_row(state.schema(), row, cells)?);
        }
        let mut attrs = table_attrs(located.table)?;
        if !attrs.column_widths.is_empty() {
            attrs.column_widths.resize(located.map.width(), None);
            attrs.column_widths.insert(column, None);
        }
        let table = rebuild_table(state.schema(), located.table, rows, Some(attrs))?;
        replace_table(
            state,
            &located,
            table,
            Some((located.cell.map(|cell| cell.top).unwrap_or(0), column)),
        )
    })
}

pub fn delete_column() -> BoxedCommand {
    Box::new(|state: &EditorState| {
        let located = locate_table(state)?;
        let column = located.cell.map(|cell| cell.left).unwrap_or(0);
        if located.map.width() == 1 {
            return delete_table().apply(state);
        }
        let mut rows = Vec::with_capacity(located.map.height());
        for row in located.table.content().iter() {
            let mut cells = row.content().iter().cloned().collect::<Vec<_>>();
            cells.remove(column);
            rows.push(rebuild_row(state.schema(), row, cells)?);
        }
        let mut attrs = table_attrs(located.table)?;
        if column < attrs.column_widths.len() {
            attrs.column_widths.remove(column);
        }
        let target_column = column.min(located.map.width() - 2);
        let table = rebuild_table(state.schema(), located.table, rows, Some(attrs))?;
        replace_table(
            state,
            &located,
            table,
            Some((
                located.cell.map(|cell| cell.top).unwrap_or(0),
                target_column,
            )),
        )
    })
}

/// Move a column to a final column index.
///
/// Every row's corresponding cell and the persisted column-width entry move
/// together. Out-of-range indices and moves to the same index are no-ops. On
/// success, the selection lands in the moved column at its previous row (or
/// row zero when the table was not selected).
pub fn move_column(source: usize, target: usize) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let located = locate_table(state)?;
        if !valid_column_move(located.map.width(), source, target) {
            return None;
        }
        let selected_row = located.cell.map(|cell| cell.top).unwrap_or(0);
        let table = moved_column_table(
            state.schema(),
            located.table,
            located.map.width(),
            table_attrs(located.table)?,
            source,
            target,
        )?;
        replace_table(state, &located, table, Some((selected_row, target)))
    })
}

pub fn set_cell_alignment(alignment: Option<TableAlignment>) -> BoxedCommand {
    set_cell_alignment_target(None, alignment)
}

pub fn set_cell_alignment_at(
    row: usize,
    column: usize,
    alignment: Option<TableAlignment>,
) -> BoxedCommand {
    set_cell_alignment_target(Some((row, column)), alignment)
}

fn set_cell_alignment_target(
    target: Option<(usize, usize)>,
    alignment: Option<TableAlignment>,
) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let located = locate_table(state)?;
        let (row, column) = target.or_else(|| located.cell.map(|cell| (cell.top, cell.left)))?;
        if row >= located.map.height() || column >= located.map.width() {
            return None;
        }
        let mut rows = located.table.content().iter().cloned().collect::<Vec<_>>();
        let source_row = &rows[row];
        let mut cells = source_row.content().iter().cloned().collect::<Vec<_>>();
        let source = &cells[column];
        let attrs = if row == 0 {
            attrs_map(&TableHeaderCellAttrs { alignment })?
        } else {
            attrs_map(&TableCellAttrs { alignment })?
        };
        cells[column] = state
            .schema()
            .node(source.type_name(), attrs, source.content().clone())
            .ok()?;
        rows[row] = rebuild_row(state.schema(), source_row, cells)?;
        let table = rebuild_table(state.schema(), located.table, rows, None)?;
        replace_table(state, &located, table, Some((row, column)))
    })
}

pub fn set_column_width(width: u32) -> BoxedCommand {
    set_column_width_target(None, width)
}

pub fn set_column_width_at(column: usize, width: u32) -> BoxedCommand {
    set_column_width_target(Some(column), width)
}

fn set_column_width_target(column: Option<usize>, width: u32) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let located = locate_table(state)?;
        let column = column.or_else(|| located.cell.map(|cell| cell.left))?;
        if column >= located.map.width() {
            return None;
        }
        let width = width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH);
        let mut attrs = table_attrs(located.table)?;
        attrs.column_widths.resize(located.map.width(), None);
        if attrs.column_widths[column] == Some(width) {
            return None;
        }
        attrs.column_widths[column] = Some(width);
        let rows = located.table.content().iter().cloned().collect();
        let table = rebuild_table(state.schema(), located.table, rows, Some(attrs))?;
        replace_table(
            state,
            &located,
            table,
            located.cell.map(|cell| (cell.top, cell.left)),
        )
    })
}

pub fn set_row_height(height: u32) -> BoxedCommand {
    set_row_height_target(None, height)
}

pub fn set_row_height_at(row: usize, height: u32) -> BoxedCommand {
    set_row_height_target(Some(row), height)
}

fn set_row_height_target(row: Option<usize>, height: u32) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let located = locate_table(state)?;
        let row = row.or_else(|| located.cell.map(|cell| cell.top))?;
        if row >= located.map.height() {
            return None;
        }
        let height = height.clamp(MIN_ROW_HEIGHT, MAX_ROW_HEIGHT);
        let mut rows = located.table.content().iter().cloned().collect::<Vec<_>>();
        let source = &rows[row];
        let current = row_attrs(source)?;
        if current.height == Some(height) {
            return None;
        }
        rows[row] = state
            .schema()
            .node(
                "table_row",
                attrs_map(&TableRowAttrs {
                    height: Some(height),
                })?,
                source.content().clone(),
            )
            .ok()?;
        let table = rebuild_table(state.schema(), located.table, rows, None)?;
        replace_table(
            state,
            &located,
            table,
            Some((row, located.cell.map(|cell| cell.left).unwrap_or(0))),
        )
    })
}

pub fn go_to_next_cell() -> BoxedCommand {
    Box::new(|state: &EditorState| {
        let located = locate_table(state)?;
        let cell = located.cell?;
        let index = cell.top * located.map.width() + cell.left;
        if index + 1 < located.map.width() * located.map.height() {
            return select_cell(state, &located, index + 1);
        }

        if located.map.height() >= MAX_TABLE_ROWS {
            return None;
        }
        let mut rows = located.table.content().iter().cloned().collect::<Vec<_>>();
        let alignments = column_alignments(located.table, located.map.width());
        rows.push(build_empty_row(state.schema(), false, &alignments)?);
        let target_row = rows.len() - 1;
        let table = rebuild_table(state.schema(), located.table, rows, None)?;
        replace_table(state, &located, table, Some((target_row, 0)))
    })
}

pub fn go_to_previous_cell() -> BoxedCommand {
    Box::new(|state: &EditorState| {
        let located = locate_table(state)?;
        let cell = located.cell?;
        let index = cell.top * located.map.width() + cell.left;
        if index == 0 {
            None
        } else {
            select_cell(state, &located, index - 1)
        }
    })
}

fn select_cell(
    state: &EditorState,
    located: &LocatedTable<'_>,
    index: usize,
) -> Option<Transaction> {
    let row = index / located.map.width();
    let column = index % located.map.width();
    let mut transaction = state.tr();
    transaction
        .set_selection(Selection::text(located.map.cell(row, column)?.pos + 1))
        .ok()?;
    Some(transaction)
}

pub(super) fn named_commands() -> Vec<(String, NamedCommand)> {
    vec![
        named("insert_table", |args| {
            let rows = usize_arg(&args, "rows").unwrap_or(3);
            let columns = usize_arg(&args, "columns")
                .or_else(|| usize_arg(&args, "cols"))
                .unwrap_or(3);
            Some(insert_table(rows, columns))
        }),
        named_no_args("insert_row_before", insert_row_before),
        named_no_args("insert_row_above", insert_row_before),
        named_no_args("insert_row_after", insert_row_after),
        named_no_args("insert_row_below", insert_row_after),
        named_no_args("delete_row", delete_row),
        named("move_row", |args| {
            Some(move_row(
                usize_arg(&args, "source")?,
                usize_arg(&args, "target")?,
            ))
        }),
        named_no_args("insert_column_before", insert_column_before),
        named_no_args("insert_column_left", insert_column_before),
        named_no_args("insert_column_after", insert_column_after),
        named_no_args("insert_column_right", insert_column_after),
        named_no_args("delete_column", delete_column),
        named("move_column", |args| {
            Some(move_column(
                usize_arg(&args, "source")?,
                usize_arg(&args, "target")?,
            ))
        }),
        named_no_args("delete_table", delete_table),
        named("set_cell_alignment", |args| {
            let alignment = match args.get("alignment") {
                None | Some(Value::Null) => None,
                Some(value) => serde_json::from_value(value.clone()).ok()?,
            };
            match (usize_arg(&args, "row"), usize_arg(&args, "column")) {
                (Some(row), Some(column)) => Some(set_cell_alignment_at(row, column, alignment)),
                (None, None) => Some(set_cell_alignment(alignment)),
                _ => None,
            }
        }),
        named("set_column_width", |args| {
            let width = args.get("width")?.as_u64()?.try_into().ok()?;
            Some(match usize_arg(&args, "column") {
                Some(column) => set_column_width_at(column, width),
                None => set_column_width(width),
            })
        }),
        named("set_row_height", |args| {
            let height = args.get("height")?.as_u64()?.try_into().ok()?;
            Some(match usize_arg(&args, "row") {
                Some(row) => set_row_height_at(row, height),
                None => set_row_height(height),
            })
        }),
    ]
}

pub(super) fn key_bindings() -> KeyBindings {
    vec![
        ("Tab".into(), Arc::new(go_to_next_cell) as KeyBindingFactory),
        (
            "Shift-Tab".into(),
            Arc::new(go_to_previous_cell) as KeyBindingFactory,
        ),
    ]
}

fn named(
    name: &str,
    factory: impl Fn(Value) -> Option<BoxedCommand> + Send + Sync + 'static,
) -> (String, NamedCommand) {
    (name.into(), Arc::new(factory))
}

fn named_no_args(name: &str, factory: fn() -> BoxedCommand) -> (String, NamedCommand) {
    named(name, move |_| Some(factory()))
}

fn usize_arg(value: &Value, key: &str) -> Option<usize> {
    value.get(key)?.as_u64()?.try_into().ok()
}

#[derive(Clone, Copy)]
enum RowSide {
    Before,
    After,
}

#[derive(Clone, Copy)]
enum ColumnSide {
    Before,
    After,
}

struct LocatedTable<'a> {
    table: &'a Node,
    map: TableMap,
    cell: Option<super::CellRect>,
}

fn locate_table(state: &EditorState) -> Option<LocatedTable<'_>> {
    let doc = state.doc();
    let selection_pos = state.selection().from(doc);
    let table_pos = if doc
        .node_at(selection_pos)
        .ok()?
        .is_some_and(|node| node.type_name() == "table")
    {
        selection_pos
    } else {
        let resolved = doc.resolve(selection_pos).ok()?;
        (1..=resolved.depth()).rev().find_map(|depth| {
            (resolved.node(depth)?.type_name() == "table")
                .then(|| resolved.before(depth))
                .flatten()
        })?
    };
    let table = doc.node_at(table_pos).ok()??;
    let map = TableMap::new(table, table_pos).ok()?;
    let cell = map.find_cell(selection_pos);
    Some(LocatedTable { table, map, cell })
}

fn nearest_table_position(doc: &Node, hint: usize) -> Option<usize> {
    let mut candidates = Vec::new();
    doc.descendants(|node, pos| {
        if node.type_name() == "table" {
            candidates.push((pos, node.node_size()));
        }
    });
    candidates
        .into_iter()
        .min_by_key(|(pos, size)| {
            if hint < *pos {
                *pos - hint
            } else {
                hint.saturating_sub(*pos + *size)
            }
        })
        .map(|(pos, _)| pos)
}

fn replace_table(
    state: &EditorState,
    located: &LocatedTable<'_>,
    table: Node,
    target: Option<(usize, usize)>,
) -> Option<Transaction> {
    let map = TableMap::new(&table, located.map.table_pos()).ok()?;
    let mut transaction = state.tr();
    transaction
        .replace_with(
            located.map.table_pos(),
            located.map.table_pos() + located.table.node_size(),
            Fragment::from(table),
        )
        .ok()?;
    if let Some((row, column)) = target {
        transaction
            .set_selection(Selection::text(map.cell(row, column)?.pos + 1))
            .ok()?;
    }
    Some(transaction)
}

fn build_empty_table(schema: &Schema, rows: usize, columns: usize) -> Option<Node> {
    let alignments = vec![None; columns];
    let mut row_nodes = Vec::with_capacity(rows);
    for row in 0..rows {
        row_nodes.push(build_empty_row(schema, row == 0, &alignments)?);
    }
    schema
        .node(
            "table",
            attrs_map(&TableAttrs::default())?,
            Fragment::from(row_nodes),
        )
        .ok()
}

fn build_empty_row(
    schema: &Schema,
    header: bool,
    alignments: &[Option<TableAlignment>],
) -> Option<Node> {
    let cells = alignments
        .iter()
        .map(|alignment| build_empty_cell(schema, header, *alignment))
        .collect::<Option<Vec<_>>>()?;
    schema
        .node(
            "table_row",
            attrs_map(&TableRowAttrs::default())?,
            Fragment::from(cells),
        )
        .ok()
}

fn build_empty_cell(
    schema: &Schema,
    header: bool,
    alignment: Option<TableAlignment>,
) -> Option<Node> {
    let (node_type, attrs) = if header {
        (
            "table_header_cell",
            attrs_map(&TableHeaderCellAttrs { alignment })?,
        )
    } else {
        ("table_cell", attrs_map(&TableCellAttrs { alignment })?)
    };
    schema.node(node_type, attrs, Fragment::empty()).ok()
}

fn rebuild_row(schema: &Schema, row: &Node, cells: Vec<Node>) -> Option<Node> {
    schema
        .node("table_row", row.attrs().clone(), Fragment::from(cells))
        .ok()
}

fn retype_row(schema: &Schema, row: &Node, header: bool) -> Option<Node> {
    let cells = row
        .content()
        .iter()
        .map(|cell| {
            schema
                .node(
                    if header {
                        "table_header_cell"
                    } else {
                        "table_cell"
                    },
                    cell.attrs().clone(),
                    cell.content().clone(),
                )
                .ok()
        })
        .collect::<Option<Vec<_>>>()?;
    rebuild_row(schema, row, cells)
}

fn rebuild_table(
    schema: &Schema,
    source: &Node,
    rows: Vec<Node>,
    attrs: Option<TableAttrs>,
) -> Option<Node> {
    let attrs = match attrs {
        Some(attrs) => attrs_map(&attrs)?,
        None => source.attrs().clone(),
    };
    schema.node("table", attrs, Fragment::from(rows)).ok()
}

fn valid_row_move(height: usize, source: usize, target: usize) -> bool {
    source > 0 && target > 0 && source < height && target < height && source != target
}

fn valid_column_move(width: usize, source: usize, target: usize) -> bool {
    source < width && target < width && source != target
}

fn moved_row_table(
    schema: &Schema,
    table: &Node,
    height: usize,
    source: usize,
    target: usize,
) -> Option<Node> {
    if !valid_row_move(height, source, target) {
        return None;
    }
    let mut rows = table.content().iter().cloned().collect::<Vec<_>>();
    move_item(&mut rows, source, target)?;
    rebuild_table(schema, table, rows, None)
}

fn moved_column_table(
    schema: &Schema,
    table: &Node,
    width: usize,
    mut attrs: TableAttrs,
    source: usize,
    target: usize,
) -> Option<Node> {
    if !valid_column_move(width, source, target) {
        return None;
    }
    let rows = table
        .content()
        .iter()
        .map(|row| {
            let mut cells = row.content().iter().cloned().collect::<Vec<_>>();
            move_item(&mut cells, source, target)?;
            rebuild_row(schema, row, cells)
        })
        .collect::<Option<Vec<_>>>()?;
    if !attrs.column_widths.is_empty() {
        attrs.column_widths.resize(width, None);
        move_item(&mut attrs.column_widths, source, target)?;
    }
    rebuild_table(schema, table, rows, Some(attrs))
}

fn move_item<T>(items: &mut Vec<T>, source: usize, target: usize) -> Option<()> {
    if source >= items.len() || target >= items.len() || source == target {
        return None;
    }
    let item = items.remove(source);
    items.insert(target, item);
    Some(())
}

fn table_attrs(table: &Node) -> Option<TableAttrs> {
    decode_attrs(table)
}

fn row_attrs(row: &Node) -> Option<TableRowAttrs> {
    decode_attrs(row)
}

fn decode_attrs<A: serde::de::DeserializeOwned>(node: &Node) -> Option<A> {
    serde_json::from_value(serde_json::to_value(node.attrs()).ok()?).ok()
}

fn attrs_map(value: &impl Serialize) -> Option<Attrs> {
    match serde_json::to_value(value).ok()? {
        Value::Object(object) => Some(object.into_iter().collect()),
        _ => None,
    }
}

fn column_alignments(table: &Node, width: usize) -> Vec<Option<TableAlignment>> {
    let mut alignments = table
        .child(0)
        .map(|row| row.content().iter().map(cell_alignment).collect::<Vec<_>>())
        .unwrap_or_default();
    alignments.resize(width, None);
    alignments
}

fn cell_alignment(cell: &Node) -> Option<TableAlignment> {
    cell.attrs()
        .get("alignment")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn nearest_alignment(cells: &[Node], insertion_column: usize) -> Option<TableAlignment> {
    insertion_column
        .checked_sub(1)
        .and_then(|index| cells.get(index))
        .or_else(|| cells.get(insertion_column))
        .and_then(cell_alignment)
}
