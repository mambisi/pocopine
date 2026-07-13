//! DOM adapter for the browser-independent table interaction state machines.

use pine_richtext::model::Fragment;
use pine_richtext::view::NodeViewSelection;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, PointerEvent};

use super::super::{
    TABLE_CELL_ATTR, TABLE_ROW_ATTR, TABLE_SELECTED_ATTR, TableAttrs, TableRowAttrs,
};
use super::controller::{
    HitRect, ResizeAxis, ResizeDrag, TableViewAction, TableViewAnchor, hit_test_resize_edge,
};
use super::grid::{TableViewGrid, TableViewGridError, ViewSelectionRect};

const EDGE_THRESHOLD_PX: f64 = 6.0;
const ROOT_STATE_ATTR: &str = "data-state";
const ROOT_SELECTION_ATTR: &str = "data-selection";
const ROOT_RESIZE_AXIS_ATTR: &str = "data-resize-axis";
const CELL_WIDTH_VAR: &str = "--pine-richtext-table-cell-width";
const ROW_HEIGHT_VAR: &str = "--pine-richtext-table-row-height";

/// Immutable model snapshot projected into one mounted table component.
#[derive(Clone, Debug)]
pub struct TableViewSnapshot {
    pub attrs: TableAttrs,
    pub content: Fragment,
    pub selection: NodeViewSelection,
    pub editable: bool,
    pub focused: bool,
}

/// Mounted table controller. It owns no document state; the typed dispatcher
/// is the only path from a gesture to an editor transaction.
pub struct TableViewController {
    root: Element,
    table: Element,
    body: Element,
    table_selector: Element,
    column_selectors: Element,
    row_selectors: Element,
    grid: TableViewGrid,
    widths: Vec<Option<u32>>,
    heights: Vec<Option<u32>>,
    selection: Option<ViewSelectionRect>,
    editable: bool,
    resize: Option<ResizeDrag>,
    selecting: Option<CellSelectionDrag>,
}

#[derive(Clone, Copy)]
struct CellSelectionDrag {
    pointer_id: i32,
    anchor_cell: usize,
}

impl TableViewController {
    #[allow(clippy::too_many_arguments)]
    pub fn attach(
        anchor: TableViewAnchor,
        root: Element,
        table: Element,
        body: Element,
        table_selector: Element,
        column_selectors: Element,
        row_selectors: Element,
        snapshot: TableViewSnapshot,
    ) -> Result<Self, TableViewControllerError> {
        let grid = TableViewGrid::from_content(&snapshot.content, anchor)?;
        let heights = row_heights(&snapshot.content)?;
        let selection = selection_rect(&grid, snapshot.selection);
        let mut controller = Self {
            root,
            table,
            body,
            table_selector,
            column_selectors,
            row_selectors,
            grid,
            widths: snapshot.attrs.column_widths,
            heights,
            selection,
            editable: snapshot.editable,
            resize: None,
            selecting: None,
        };
        controller.paint(snapshot.focused)?;
        Ok(controller)
    }

    /// Retain this controller and project a new semantic snapshot into the DOM.
    pub fn sync(
        &mut self,
        anchor: TableViewAnchor,
        snapshot: TableViewSnapshot,
    ) -> Result<(), TableViewControllerError> {
        let active_pointer = self
            .resize
            .map(ResizeDrag::pointer_id)
            .or_else(|| self.selecting.map(|selection| selection.pointer_id));
        if let Some(pointer_id) = active_pointer {
            self.release_pointer(pointer_id);
            self.clear_interaction_state()?;
        }
        self.grid = TableViewGrid::from_content(&snapshot.content, anchor)?;
        self.widths = snapshot.attrs.column_widths;
        self.heights = row_heights(&snapshot.content)?;
        self.selection = selection_rect(&self.grid, snapshot.selection);
        self.editable = snapshot.editable;
        self.resize = None;
        self.selecting = None;
        self.paint(snapshot.focused)
    }

    pub fn pointer_down(&mut self, event: PointerEvent) -> Result<bool, TableViewControllerError> {
        if !self.editable || event.button() != 0 {
            return Ok(false);
        }
        let Some(cell) = event_cell(&event, &self.table) else {
            return Ok(false);
        };
        let (row, column) = cell_coordinate(&cell, &self.body)?;

        let rect = cell.get_bounding_client_rect();
        let edge = hit_test_resize_edge(
            HitRect {
                left: rect.left(),
                top: rect.top(),
                width: rect.width(),
                height: rect.height(),
            },
            event.client_x() as f64,
            event.client_y() as f64,
            row,
            column,
            EDGE_THRESHOLD_PX,
        );
        if let Some(edge) = edge {
            let coordinate = axis_coordinate(edge.axis, &event);
            let start_size = match edge.axis {
                ResizeAxis::Column => rect.width(),
                ResizeAxis::Row => rect.height(),
            };
            let anchor = self.grid.anchor();
            let Some(resize) =
                ResizeDrag::begin(anchor, event.pointer_id(), edge, coordinate, start_size)
            else {
                return Ok(false);
            };
            self.table
                .set_pointer_capture(event.pointer_id())
                .map_err(dom_error)?;
            self.resize = Some(resize);
            self.root
                .set_attribute(ROOT_STATE_ATTR, "resizing")
                .map_err(dom_error)?;
            self.root
                .set_attribute(
                    ROOT_RESIZE_AXIS_ATTR,
                    match edge.axis {
                        ResizeAxis::Column => "column",
                        ResizeAxis::Row => "row",
                    },
                )
                .map_err(dom_error)?;
            event.prevent_default();
            return Ok(true);
        }

        // Text editing keeps ordinary pointer semantics. Mod/Shift drag is the
        // explicit rectangular-cell-selection gesture; row/column/table chrome
        // remains available without modifiers.
        if event.shift_key() || event.ctrl_key() || event.meta_key() {
            let Some(cell) = self.grid.cell(row, column) else {
                return Ok(false);
            };
            self.table
                .set_pointer_capture(event.pointer_id())
                .map_err(dom_error)?;
            self.selecting = Some(CellSelectionDrag {
                pointer_id: event.pointer_id(),
                anchor_cell: cell.pos,
            });
            self.selection = self.grid.selection_rect(cell.pos, cell.pos);
            self.paint_selection()?;
            event.prevent_default();
            return Ok(true);
        }
        Ok(false)
    }

    pub fn pointer_move(&mut self, event: PointerEvent) -> Result<bool, TableViewControllerError> {
        if let Some(mut resize) = self.resize.take() {
            let coordinate = axis_coordinate(resize.edge().axis, &event);
            let Some(size) = resize.update(event.pointer_id(), coordinate) else {
                self.resize = Some(resize);
                return Ok(false);
            };
            let edge = resize.edge();
            self.resize = Some(resize);
            self.paint_resize_preview(edge.axis, edge.index, size)?;
            event.prevent_default();
            return Ok(true);
        }
        let Some(selection) = self.selecting else {
            return Ok(false);
        };
        if selection.pointer_id != event.pointer_id() {
            return Ok(false);
        }
        let Some(cell) = pointer_cell(&event, &self.table) else {
            return Ok(false);
        };
        let (row, column) = cell_coordinate(&cell, &self.body)?;
        let Some(head) = self.grid.cell(row, column) else {
            return Ok(false);
        };
        self.selection = self.grid.selection_rect(selection.anchor_cell, head.pos);
        self.paint_selection()?;
        event.prevent_default();
        Ok(true)
    }

    pub fn pointer_up(
        &mut self,
        event: PointerEvent,
    ) -> Result<Option<TableViewAction>, TableViewControllerError> {
        if let Some(resize) = self.resize.take() {
            if resize.pointer_id() != event.pointer_id() {
                self.resize = Some(resize);
                return Ok(None);
            }
            self.release_pointer(event.pointer_id());
            self.clear_interaction_state()?;
            if let Some(commit) = resize.finish(event.pointer_id()) {
                event.prevent_default();
                return Ok(Some(TableViewAction::Resize(commit)));
            } else {
                self.paint_geometry()?;
            }
            event.prevent_default();
            return Ok(None);
        }
        let Some(selection) = self.selecting.take() else {
            return Ok(None);
        };
        if selection.pointer_id != event.pointer_id() {
            self.selecting = Some(selection);
            return Ok(None);
        }
        self.release_pointer(event.pointer_id());
        let anchor_cell = selection.anchor_cell;
        let head_cell = pointer_cell(&event, &self.table)
            .and_then(|cell| cell_coordinate(&cell, &self.body).ok())
            .and_then(|(row, column)| self.grid.cell(row, column))
            .map_or(anchor_cell, |cell| cell.pos);
        let commit = self
            .grid
            .selection_rect(anchor_cell, head_cell)
            .and_then(|_| {
                let anchor = self.grid.coordinate_at(anchor_cell)?;
                let head = self.grid.coordinate_at(head_cell)?;
                Some(super::controller::CellSelectionCommit {
                    anchor: self.grid.anchor(),
                    anchor_row: anchor.row,
                    anchor_column: anchor.column,
                    head_row: head.row,
                    head_column: head.column,
                })
            });
        event.prevent_default();
        Ok(commit.map(TableViewAction::Select))
    }

    /// Pointer cancel is intentionally transaction-free.
    pub fn pointer_cancel(
        &mut self,
        event: PointerEvent,
    ) -> Result<bool, TableViewControllerError> {
        let owned = self
            .resize
            .is_some_and(|resize| resize.pointer_id() == event.pointer_id())
            || self
                .selecting
                .is_some_and(|selection| selection.pointer_id == event.pointer_id());
        if !owned {
            return Ok(false);
        }
        self.resize = None;
        self.selecting = None;
        self.release_pointer(event.pointer_id());
        self.clear_interaction_state()?;
        self.paint_geometry()?;
        self.paint_selection()?;
        event.prevent_default();
        Ok(true)
    }

    pub fn select_cell(
        &self,
        row: usize,
        column: usize,
    ) -> Result<TableViewAction, TableViewControllerError> {
        let commit = self
            .grid
            .select_cell(row, column)
            .ok_or(TableViewControllerError::CellOutside { row, column })?;
        Ok(TableViewAction::Select(commit))
    }

    pub fn select_row(&self, row: usize) -> Result<TableViewAction, TableViewControllerError> {
        let commit = self
            .grid
            .select_row(row)
            .ok_or(TableViewControllerError::CellOutside { row, column: 0 })?;
        Ok(TableViewAction::Select(commit))
    }

    pub fn select_column(
        &self,
        column: usize,
    ) -> Result<TableViewAction, TableViewControllerError> {
        let commit = self
            .grid
            .select_column(column)
            .ok_or(TableViewControllerError::CellOutside { row: 0, column })?;
        Ok(TableViewAction::Select(commit))
    }

    pub fn select_table(&self) -> TableViewAction {
        TableViewAction::Select(self.grid.select_table())
    }

    pub fn grid(&self) -> &TableViewGrid {
        &self.grid
    }

    fn paint(&mut self, focused: bool) -> Result<(), TableViewControllerError> {
        self.root
            .set_attribute("data-focused", if focused { "true" } else { "false" })
            .map_err(dom_error)?;
        self.root
            .set_attribute(
                "data-editable",
                if self.editable { "true" } else { "false" },
            )
            .map_err(dom_error)?;
        self.paint_geometry()?;
        self.paint_selection()
    }

    fn paint_geometry(&self) -> Result<(), TableViewControllerError> {
        for row in 0..self.grid.height() {
            let Some(row_element) = row_element(&self.body, row) else {
                continue;
            };
            let height = self.heights.get(row).copied().flatten();
            set_size_var(&row_element, ROW_HEIGHT_VAR, height)?;
            for column in 0..self.grid.width() {
                let Some(cell) = cell_element(&row_element, column) else {
                    continue;
                };
                let width = self.widths.get(column).copied().flatten();
                set_size_var(&cell, CELL_WIDTH_VAR, width)?;
            }
        }
        Ok(())
    }

    fn paint_resize_preview(
        &self,
        axis: ResizeAxis,
        index: usize,
        size: u32,
    ) -> Result<(), TableViewControllerError> {
        match axis {
            ResizeAxis::Column => {
                for row in 0..self.grid.height() {
                    let Some(row_element) = row_element(&self.body, row) else {
                        continue;
                    };
                    if let Some(cell) = cell_element(&row_element, index) {
                        set_size_var(&cell, CELL_WIDTH_VAR, Some(size))?;
                    }
                }
            }
            ResizeAxis::Row => {
                if let Some(row) = row_element(&self.body, index) {
                    set_size_var(&row, ROW_HEIGHT_VAR, Some(size))?;
                }
            }
        }
        Ok(())
    }

    fn paint_selection(&self) -> Result<(), TableViewControllerError> {
        let kind = selection_kind(self.selection, self.grid.width(), self.grid.height());
        self.root
            .set_attribute(ROOT_SELECTION_ATTR, kind)
            .map_err(dom_error)?;
        let table_selected = kind == "table";
        set_selected(&self.table_selector, table_selected)?;
        self.table_selector
            .set_attribute(
                "aria-pressed",
                if table_selected { "true" } else { "false" },
            )
            .map_err(dom_error)?;
        for row in 0..self.grid.height() {
            let Some(row_element) = row_element(&self.body, row) else {
                continue;
            };
            let row_selected = self
                .selection
                .is_some_and(|rect| rect.covers_row(row, self.grid.width()));
            set_selected(&row_element, row_selected)?;
            for column in 0..self.grid.width() {
                let Some(cell) = cell_element(&row_element, column) else {
                    continue;
                };
                let selected = self
                    .selection
                    .is_some_and(|rect| rect.contains(row, column));
                set_selected(&cell, selected)?;
                cell.set_attribute("aria-selected", if selected { "true" } else { "false" })
                    .map_err(dom_error)?;
            }
        }
        paint_selector_buttons(
            &self.column_selectors,
            self.selection,
            true,
            self.grid.width(),
            self.grid.height(),
        )?;
        paint_selector_buttons(
            &self.row_selectors,
            self.selection,
            false,
            self.grid.width(),
            self.grid.height(),
        )?;
        Ok(())
    }

    fn release_pointer(&self, pointer_id: i32) {
        if self.table.has_pointer_capture(pointer_id) {
            let _ = self.table.release_pointer_capture(pointer_id);
        }
    }

    fn clear_interaction_state(&self) -> Result<(), TableViewControllerError> {
        self.root
            .set_attribute(ROOT_STATE_ATTR, "ready")
            .map_err(dom_error)?;
        self.root
            .remove_attribute(ROOT_RESIZE_AXIS_ATTR)
            .map_err(dom_error)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableViewControllerError {
    Grid(TableViewGridError),
    Attrs(String),
    Dom(String),
    CellOutside { row: usize, column: usize },
}

impl std::fmt::Display for TableViewControllerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grid(error) => error.fmt(formatter),
            Self::Attrs(message) => write!(formatter, "invalid table view attrs: {message}"),
            Self::Dom(message) => write!(formatter, "table DOM update failed: {message}"),
            Self::CellOutside { row, column } => {
                write!(
                    formatter,
                    "table cell {row},{column} is outside the live grid"
                )
            }
        }
    }
}

impl std::error::Error for TableViewControllerError {}

impl From<TableViewGridError> for TableViewControllerError {
    fn from(error: TableViewGridError) -> Self {
        Self::Grid(error)
    }
}

fn row_heights(content: &Fragment) -> Result<Vec<Option<u32>>, TableViewControllerError> {
    content
        .iter()
        .map(|row| {
            serde_json::from_value::<TableRowAttrs>(
                serde_json::to_value(row.attrs())
                    .map_err(|error| TableViewControllerError::Attrs(error.to_string()))?,
            )
            .map(|attrs| attrs.height)
            .map_err(|error| TableViewControllerError::Attrs(error.to_string()))
        })
        .collect()
}

fn selection_rect(grid: &TableViewGrid, selection: NodeViewSelection) -> Option<ViewSelectionRect> {
    match selection {
        NodeViewSelection::Cells {
            anchor_cell,
            head_cell,
        } => grid.selection_rect(anchor_cell, head_cell),
        _ => None,
    }
}

fn event_cell(event: &PointerEvent, table: &Element) -> Option<Element> {
    cell_from_element(event.target()?.dyn_into::<Element>().ok(), table)
}

fn pointer_cell(event: &PointerEvent, table: &Element) -> Option<Element> {
    event_cell(event, table).or_else(|| {
        let element = table
            .owner_document()?
            .element_from_point(event.client_x() as f32, event.client_y() as f32)?;
        cell_from_element(Some(element), table)
    })
}

fn cell_from_element(mut current: Option<Element>, table: &Element) -> Option<Element> {
    while let Some(element) = current {
        if element.has_attribute(TABLE_CELL_ATTR) {
            return Some(element);
        }
        if element == *table {
            return None;
        }
        current = element.parent_element();
    }
    None
}

fn axis_coordinate(axis: ResizeAxis, event: &PointerEvent) -> f64 {
    match axis {
        ResizeAxis::Column => event.client_x() as f64,
        ResizeAxis::Row => event.client_y() as f64,
    }
}

fn row_element(body: &Element, row: usize) -> Option<Element> {
    let element = body.children().item(row.try_into().ok()?)?;
    element.has_attribute(TABLE_ROW_ATTR).then_some(element)
}

fn cell_element(row: &Element, column: usize) -> Option<Element> {
    let element = row.children().item(column.try_into().ok()?)?;
    element.has_attribute(TABLE_CELL_ATTR).then_some(element)
}

fn cell_coordinate(
    cell: &Element,
    body: &Element,
) -> Result<(usize, usize), TableViewControllerError> {
    let row = cell
        .parent_element()
        .ok_or_else(|| TableViewControllerError::Dom("table cell has no row parent".into()))?;
    if !row.has_attribute(TABLE_ROW_ATTR) || row.parent_element().as_ref() != Some(body) {
        return Err(TableViewControllerError::Dom(
            "table cell is outside the owned-content row grid".into(),
        ));
    }
    let row_index = element_index(body, &row).ok_or_else(|| {
        TableViewControllerError::Dom("table row is absent from its rowgroup".into())
    })?;
    let column_index = element_index(&row, cell)
        .ok_or_else(|| TableViewControllerError::Dom("table cell is absent from its row".into()))?;
    Ok((row_index, column_index))
}

fn element_index(parent: &Element, target: &Element) -> Option<usize> {
    let children = parent.children();
    (0..children.length()).find_map(|index| {
        children
            .item(index)
            .is_some_and(|child| child == *target)
            .then_some(index as usize)
    })
}

fn set_size_var(
    element: &Element,
    name: &str,
    size: Option<u32>,
) -> Result<(), TableViewControllerError> {
    let html = element
        .clone()
        .dyn_into::<HtmlElement>()
        .map_err(|_| TableViewControllerError::Dom("table host is not an HtmlElement".into()))?;
    let style = html.style();
    if let Some(size) = size {
        style
            .set_property(name, &format!("{size}px"))
            .map_err(dom_error)
    } else {
        style.remove_property(name).map(|_| ()).map_err(dom_error)
    }
}

fn set_selected(element: &Element, selected: bool) -> Result<(), TableViewControllerError> {
    element
        .set_attribute(TABLE_SELECTED_ATTR, if selected { "true" } else { "false" })
        .map_err(dom_error)
}

fn paint_selector_buttons(
    container: &Element,
    selection: Option<ViewSelectionRect>,
    columns: bool,
    width: usize,
    height: usize,
) -> Result<(), TableViewControllerError> {
    let children = container.children();
    for index in 0..children.length() {
        let Some(button) = children.item(index) else {
            continue;
        };
        let logical = index as usize;
        let selected = selection.is_some_and(|rect| {
            if columns {
                rect.covers_column(logical, height)
            } else {
                rect.covers_row(logical, width)
            }
        });
        set_selected(&button, selected)?;
        button
            .set_attribute("aria-pressed", if selected { "true" } else { "false" })
            .map_err(dom_error)?;
    }
    Ok(())
}

fn selection_kind(
    selection: Option<ViewSelectionRect>,
    width: usize,
    height: usize,
) -> &'static str {
    let Some(rect) = selection else {
        return "none";
    };
    if rect.left == 0 && rect.right == width && rect.top == 0 && rect.bottom == height {
        "table"
    } else if rect.left == 0 && rect.right == width {
        "row"
    } else if rect.top == 0 && rect.bottom == height {
        "column"
    } else {
        "cells"
    }
}

fn dom_error(error: wasm_bindgen::JsValue) -> TableViewControllerError {
    TableViewControllerError::Dom(format!("{error:?}"))
}
