//! DOM adapter for the browser-independent table interaction state machines.

use pine_richtext::model::Fragment;
use pine_richtext::view::NodeViewSelection;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, PointerEvent};

use super::super::{
    TABLE_CELL_ATTR, TABLE_ROW_ATTR, TABLE_SELECTED_ATTR, TableAttrs, TableRowAttrs,
};
use super::controller::{
    HitRect, MoveAxis, MoveDrag, ResizeAxis, ResizeDrag, TableViewAction, TableViewAnchor,
    hit_test_resize_edge,
};
use super::grid::{TableViewGrid, TableViewGridError, ViewSelectionRect};

const EDGE_THRESHOLD_PX: f64 = 6.0;
const ROOT_STATE_ATTR: &str = "data-state";
const ROOT_ACTIVE_ATTR: &str = "data-active";
const ROOT_SELECTION_ATTR: &str = "data-selection";
const ROOT_RESIZE_AXIS_ATTR: &str = "data-resize-axis";
const ROOT_MOVE_AXIS_ATTR: &str = "data-move-axis";
const MOVE_SOURCE_ATTR: &str = "data-move-source";
const MOVE_TARGET_ATTR: &str = "data-move-target";
const CELL_WIDTH_VAR: &str = "--pine-richtext-table-cell-width";
const ROW_HEIGHT_VAR: &str = "--pine-richtext-table-row-height";
const HANDLE_X_VAR: &str = "--pine-richtext-table-handle-x";
const HANDLE_Y_VAR: &str = "--pine-richtext-table-handle-y";
const MOVE_SOURCE_SLOP_PX: f64 = 8.0;

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
    reorder_actions: Element,
    reorder_backward: Element,
    reorder_forward: Element,
    grid: TableViewGrid,
    widths: Vec<Option<u32>>,
    heights: Vec<Option<u32>>,
    selection: Option<ViewSelectionRect>,
    selection_dismissed: bool,
    editable: bool,
    resize: Option<ResizeDrag>,
    selecting: Option<CellSelectionDrag>,
    moving: Option<MoveDrag>,
    painted_move: Option<(MoveAxis, usize, usize)>,
    pointer_capture: Option<Element>,
    suppressed_click: Option<(MoveAxis, usize)>,
}

#[derive(Clone, Copy)]
struct CellSelectionDrag {
    pointer_id: i32,
    anchor_cell: usize,
    initial_selection: Option<ViewSelectionRect>,
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
        reorder_actions: Element,
        reorder_backward: Element,
        reorder_forward: Element,
        snapshot: TableViewSnapshot,
    ) -> Result<Self, TableViewControllerError> {
        let grid = TableViewGrid::from_content(&snapshot.content, anchor)?;
        let heights = row_heights(&snapshot.content)?;
        let active = snapshot.selection != NodeViewSelection::Outside;
        let selection = selection_rect(&grid, snapshot.selection);
        let mut controller = Self {
            root,
            table,
            body,
            table_selector,
            column_selectors,
            row_selectors,
            reorder_actions,
            reorder_backward,
            reorder_forward,
            grid,
            widths: snapshot.attrs.column_widths,
            heights,
            selection,
            selection_dismissed: false,
            editable: snapshot.editable,
            resize: None,
            selecting: None,
            moving: None,
            painted_move: None,
            pointer_capture: None,
            suppressed_click: None,
        };
        controller.paint(snapshot.focused, active)?;
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
            .or_else(|| self.selecting.map(|selection| selection.pointer_id))
            .or_else(|| self.moving.map(MoveDrag::pointer_id));
        if let Some(pointer_id) = active_pointer {
            self.release_pointer(pointer_id);
            self.clear_interaction_state()?;
        }
        self.grid = TableViewGrid::from_content(&snapshot.content, anchor)?;
        self.widths = snapshot.attrs.column_widths;
        self.heights = row_heights(&snapshot.content)?;
        let selection = selection_rect(&self.grid, snapshot.selection);
        if selection != self.selection {
            self.selection_dismissed = false;
        }
        self.selection = selection;
        let active = snapshot.selection != NodeViewSelection::Outside && !self.selection_dismissed;
        self.editable = snapshot.editable;
        self.resize = None;
        self.selecting = None;
        self.moving = None;
        self.painted_move = None;
        self.paint(snapshot.focused, active)
    }

    pub fn pointer_down(&mut self, event: PointerEvent) -> Result<bool, TableViewControllerError> {
        if !self.editable
            || event.button() != 0
            || self.resize.is_some()
            || self.selecting.is_some()
            || self.moving.is_some()
        {
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
            self.pointer_capture = Some(self.table.clone());
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
            self.pointer_capture = Some(self.table.clone());
            self.selecting = Some(CellSelectionDrag {
                pointer_id: event.pointer_id(),
                anchor_cell: cell.pos,
                initial_selection: self.selection,
            });
            self.selection_dismissed = false;
            self.selection = self.grid.selection_rect(cell.pos, cell.pos);
            self.paint_selection()?;
            event.prevent_default();
            return Ok(true);
        }
        Ok(false)
    }

    /// Begin a row or column move from its overlay handle.
    ///
    /// The header row remains selectable but is deliberately rejected as a
    /// move source. Pointer capture stays on the handle so an undragged press
    /// still produces the button's normal click activation.
    pub fn pointer_down_move(
        &mut self,
        event: PointerEvent,
        axis: MoveAxis,
        source: usize,
    ) -> Result<bool, TableViewControllerError> {
        self.suppressed_click = None;
        if !self.editable
            || event.button() != 0
            || self.resize.is_some()
            || self.selecting.is_some()
            || self.moving.is_some()
            || !self.move_index_in_bounds(axis, source)
        {
            return Ok(false);
        }
        let coordinate = move_coordinate(axis, &event);
        self.restore_selection()?;
        let Some(moving) = MoveDrag::begin(
            self.grid.anchor(),
            event.pointer_id(),
            axis,
            source,
            coordinate,
        ) else {
            return Ok(false);
        };
        let handle = event
            .current_target()
            .and_then(|target| target.dyn_into::<Element>().ok())
            .ok_or_else(|| {
                TableViewControllerError::Dom("table move handle is unavailable".into())
            })?;
        handle
            .set_pointer_capture(event.pointer_id())
            .map_err(dom_error)?;
        self.pointer_capture = Some(handle);
        self.moving = Some(moving);
        Ok(true)
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
        if let Some(mut moving) = self.moving.take() {
            let axis = moving.axis();
            let coordinate = move_coordinate(axis, &event);
            let Some(target) = self.move_target(axis, moving.source(), coordinate) else {
                self.moving = Some(moving);
                return Ok(false);
            };
            if !moving.update(event.pointer_id(), coordinate, target) {
                self.moving = Some(moving);
                return Ok(false);
            }
            if moving.is_active() {
                self.paint_move_preview(axis, moving.source(), moving.target())?;
                event.prevent_default();
            }
            self.moving = Some(moving);
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
                self.paint_handle_geometry()?;
            }
            event.prevent_default();
            return Ok(None);
        }
        if let Some(moving) = self.moving.take() {
            if moving.pointer_id() != event.pointer_id() {
                self.moving = Some(moving);
                return Ok(None);
            }
            let active = moving.is_active();
            if active {
                self.suppressed_click = Some((moving.axis(), moving.source()));
            }
            self.release_pointer(event.pointer_id());
            self.clear_interaction_state()?;
            if active {
                event.prevent_default();
            }
            return Ok(moving.finish(event.pointer_id()).map(TableViewAction::Move));
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
                .is_some_and(|selection| selection.pointer_id == event.pointer_id())
            || self
                .moving
                .is_some_and(|moving| moving.pointer_id() == event.pointer_id());
        if !owned {
            return Ok(false);
        }
        self.resize = None;
        if let Some(selecting) = self.selecting.take() {
            self.selection = selecting.initial_selection;
        }
        self.moving = None;
        self.release_pointer(event.pointer_id());
        self.clear_interaction_state()?;
        self.paint_geometry()?;
        self.paint_selection()?;
        self.paint_handle_geometry()?;
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

    /// Build a one-step move for the selected single row or column.
    ///
    /// The contextual buttons call this path, providing a click and keyboard
    /// alternative to dragging without adding permanent table gutters.
    pub fn move_selected(&self, forward: bool) -> Option<TableViewAction> {
        if !self.editable {
            return None;
        }
        let (axis, source) = self.selected_move_item()?;
        let target = if forward {
            source.checked_add(1)?
        } else {
            source.checked_sub(1)?
        };
        if !self.move_index_in_bounds(axis, source)
            || !self.move_index_in_bounds(axis, target)
            || source == target
        {
            return None;
        }
        Some(TableViewAction::Move(super::controller::MoveCommit {
            anchor: self.grid.anchor(),
            axis,
            source,
            target,
        }))
    }

    /// Consume the pointer-generated click that follows an activated drag.
    /// Keyboard clicks (`detail == 0`) bypass this path in the component.
    pub fn consume_suppressed_click(&mut self, axis: MoveAxis, index: usize) -> bool {
        if self.suppressed_click == Some((axis, index)) {
            self.suppressed_click = None;
            true
        } else {
            false
        }
    }

    pub fn grid(&self) -> &TableViewGrid {
        &self.grid
    }

    pub fn refresh_handle_geometry(&self) -> Result<(), TableViewControllerError> {
        self.paint_handle_geometry()
    }

    pub fn refresh_anchor(&mut self, anchor: TableViewAnchor) {
        self.grid.set_anchor(anchor);
    }

    /// Hide rectangular selection chrome after a pointer interaction outside
    /// the table without discarding the editor's semantic selection. External
    /// toolbar commands can still act on the selected cells; the next table
    /// interaction or a different semantic selection restores normal paint.
    pub fn dismiss_selection(&mut self) -> Result<(), TableViewControllerError> {
        if self.selection.is_none() || self.selection_dismissed {
            return Ok(());
        }
        self.selection_dismissed = true;
        self.root
            .set_attribute(ROOT_ACTIVE_ATTR, "false")
            .map_err(dom_error)?;
        self.paint_selection()
    }

    pub fn restore_selection(&mut self) -> Result<(), TableViewControllerError> {
        if !self.selection_dismissed {
            return Ok(());
        }
        self.selection_dismissed = false;
        self.root
            .set_attribute(
                ROOT_ACTIVE_ATTR,
                if self.selection.is_some() {
                    "true"
                } else {
                    "false"
                },
            )
            .map_err(dom_error)?;
        self.paint_selection()
    }

    pub fn focus_reorder_action(
        &self,
        prefer_forward: bool,
    ) -> Result<(), TableViewControllerError> {
        let (preferred, fallback) = if prefer_forward {
            (&self.reorder_forward, &self.reorder_backward)
        } else {
            (&self.reorder_backward, &self.reorder_forward)
        };
        if !preferred.has_attribute("disabled") {
            focus_element(preferred)
        } else if !fallback.has_attribute("disabled") {
            focus_element(fallback)
        } else {
            Ok(())
        }
    }

    fn paint(&mut self, focused: bool, active: bool) -> Result<(), TableViewControllerError> {
        self.root
            .set_attribute("data-focused", if focused { "true" } else { "false" })
            .map_err(dom_error)?;
        self.root
            .set_attribute(ROOT_ACTIVE_ATTR, if active { "true" } else { "false" })
            .map_err(dom_error)?;
        self.root
            .set_attribute(
                "data-editable",
                if self.editable { "true" } else { "false" },
            )
            .map_err(dom_error)?;
        self.paint_geometry()?;
        self.paint_selection()?;
        self.paint_handle_geometry()
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
        self.paint_handle_geometry()
    }

    fn paint_handle_geometry(&self) -> Result<(), TableViewControllerError> {
        let root_rect = self.root.get_bounding_client_rect();
        let columns = self.column_selectors.children();
        for column in 0..self.grid.width() {
            let Some(button) = columns.item(column as u32) else {
                continue;
            };
            let Some(header) =
                row_element(&self.body, 0).and_then(|row| cell_element(&row, column))
            else {
                continue;
            };
            let rect = header.get_bounding_client_rect();
            let center = rect.left() + rect.width() / 2.0;
            let visible = center >= root_rect.left() && center <= root_rect.right();
            if visible {
                button.remove_attribute("hidden").map_err(dom_error)?;
                set_pixel_var(&button, HANDLE_X_VAR, center - root_rect.left())?;
            } else {
                button.set_attribute("hidden", "").map_err(dom_error)?;
            }
        }
        let rows = self.row_selectors.children();
        for row in 0..self.grid.height() {
            let Some(button) = rows.item(row as u32) else {
                continue;
            };
            let Some(row_element) = row_element(&self.body, row) else {
                continue;
            };
            let rect = row_element.get_bounding_client_rect();
            set_pixel_var(
                &button,
                HANDLE_Y_VAR,
                rect.top() - root_rect.top() + rect.height() / 2.0,
            )?;
        }
        Ok(())
    }

    fn paint_move_preview(
        &mut self,
        axis: MoveAxis,
        source: usize,
        target: usize,
    ) -> Result<(), TableViewControllerError> {
        if self.painted_move == Some((axis, source, target)) {
            return Ok(());
        }
        self.clear_move_marks()?;
        self.painted_move = Some((axis, source, target));
        self.root
            .set_attribute(ROOT_STATE_ATTR, "moving")
            .map_err(dom_error)?;
        self.root
            .set_attribute(
                ROOT_MOVE_AXIS_ATTR,
                match axis {
                    MoveAxis::Column => "column",
                    MoveAxis::Row => "row",
                },
            )
            .map_err(dom_error)?;

        match axis {
            MoveAxis::Row => {
                if let Some(row) = row_element(&self.body, source) {
                    set_marker(&row, MOVE_SOURCE_ATTR, "true")?;
                }
                if let Some(button) = self.row_selectors.children().item(source as u32) {
                    set_marker(&button, MOVE_SOURCE_ATTR, "true")?;
                }
                if target != source {
                    let edge = if target < source { "before" } else { "after" };
                    if let Some(row) = row_element(&self.body, target) {
                        set_marker(&row, MOVE_TARGET_ATTR, edge)?;
                    }
                    if let Some(button) = self.row_selectors.children().item(target as u32) {
                        set_marker(&button, MOVE_TARGET_ATTR, edge)?;
                    }
                }
            }
            MoveAxis::Column => {
                for row in 0..self.grid.height() {
                    let Some(row_element) = row_element(&self.body, row) else {
                        continue;
                    };
                    if let Some(cell) = cell_element(&row_element, source) {
                        set_marker(&cell, MOVE_SOURCE_ATTR, "true")?;
                    }
                    if target != source {
                        let edge = if target < source { "before" } else { "after" };
                        if let Some(cell) = cell_element(&row_element, target) {
                            set_marker(&cell, MOVE_TARGET_ATTR, edge)?;
                        }
                    }
                }
                if let Some(button) = self.column_selectors.children().item(source as u32) {
                    set_marker(&button, MOVE_SOURCE_ATTR, "true")?;
                }
                if target != source {
                    let edge = if target < source { "before" } else { "after" };
                    if let Some(button) = self.column_selectors.children().item(target as u32) {
                        set_marker(&button, MOVE_TARGET_ATTR, edge)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn paint_selection(&self) -> Result<(), TableViewControllerError> {
        let visible_selection = (!self.selection_dismissed)
            .then_some(self.selection)
            .flatten();
        let kind = selection_kind(visible_selection, self.grid.width(), self.grid.height());
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
            let row_selected =
                visible_selection.is_some_and(|rect| rect.covers_row(row, self.grid.width()));
            set_selected(&row_element, row_selected)?;
            for column in 0..self.grid.width() {
                let Some(cell) = cell_element(&row_element, column) else {
                    continue;
                };
                let selected = visible_selection.is_some_and(|rect| rect.contains(row, column));
                set_selected(&cell, selected)?;
                cell.set_attribute("aria-selected", if selected { "true" } else { "false" })
                    .map_err(dom_error)?;
            }
        }
        paint_selector_buttons(
            &self.column_selectors,
            visible_selection,
            true,
            self.grid.width(),
            self.grid.height(),
        )?;
        paint_selector_buttons(
            &self.row_selectors,
            visible_selection,
            false,
            self.grid.width(),
            self.grid.height(),
        )?;
        self.paint_reorder_actions()
    }

    fn paint_reorder_actions(&self) -> Result<(), TableViewControllerError> {
        if !self.editable || self.selection_dismissed {
            self.reorder_actions
                .set_attribute("hidden", "")
                .map_err(dom_error)?;
            return Ok(());
        }
        let Some((axis, source)) = self.selected_move_item() else {
            self.reorder_actions
                .set_attribute("hidden", "")
                .map_err(dom_error)?;
            return Ok(());
        };
        let axis_name = match axis {
            MoveAxis::Column => "column",
            MoveAxis::Row => "row",
        };
        self.reorder_actions
            .set_attribute("data-axis", axis_name)
            .map_err(dom_error)?;
        self.reorder_actions
            .set_attribute("aria-label", &format!("Reorder selected {axis_name}"))
            .map_err(dom_error)?;
        let backward_label = match axis {
            MoveAxis::Column => "Move selected column to its previous position",
            MoveAxis::Row => "Move selected row up",
        };
        let forward_label = match axis {
            MoveAxis::Column => "Move selected column to its next position",
            MoveAxis::Row => "Move selected row down",
        };
        self.reorder_backward
            .set_attribute("aria-label", backward_label)
            .map_err(dom_error)?;
        self.reorder_forward
            .set_attribute("aria-label", forward_label)
            .map_err(dom_error)?;
        let backward_disabled = !source
            .checked_sub(1)
            .is_some_and(|target| self.move_index_in_bounds(axis, target));
        let forward_disabled = !source
            .checked_add(1)
            .is_some_and(|target| self.move_index_in_bounds(axis, target));
        self.preserve_reorder_focus(backward_disabled, forward_disabled)?;
        set_disabled(&self.reorder_backward, backward_disabled)?;
        set_disabled(&self.reorder_forward, forward_disabled)?;
        self.reorder_actions
            .remove_attribute("hidden")
            .map(|_| ())
            .map_err(dom_error)
    }

    fn selected_move_item(&self) -> Option<(MoveAxis, usize)> {
        let selection = self.selection?;
        if selection.left == 0
            && selection.right == self.grid.width()
            && selection.bottom == selection.top + 1
            && selection.top > 0
            && self.grid.height() > 2
        {
            return Some((MoveAxis::Row, selection.top));
        }
        if selection.top == 0
            && selection.bottom == self.grid.height()
            && selection.right == selection.left + 1
            && self.grid.width() > 1
        {
            return Some((MoveAxis::Column, selection.left));
        }
        None
    }

    fn preserve_reorder_focus(
        &self,
        backward_disabled: bool,
        forward_disabled: bool,
    ) -> Result<(), TableViewControllerError> {
        let active = self
            .root
            .owner_document()
            .and_then(|document| document.active_element());
        if backward_disabled && !forward_disabled && active.as_ref() == Some(&self.reorder_backward)
        {
            return focus_element(&self.reorder_forward);
        }
        if forward_disabled && !backward_disabled && active.as_ref() == Some(&self.reorder_forward)
        {
            return focus_element(&self.reorder_backward);
        }
        Ok(())
    }

    fn move_index_in_bounds(&self, axis: MoveAxis, index: usize) -> bool {
        match axis {
            MoveAxis::Column => self.grid.width() > 1 && index < self.grid.width(),
            MoveAxis::Row => self.grid.height() > 2 && index > 0 && index < self.grid.height(),
        }
    }

    fn move_target(&self, axis: MoveAxis, source: usize, coordinate: f64) -> Option<usize> {
        if !coordinate.is_finite() {
            return None;
        }
        let source_rect = self.move_item_rect(axis, source)?;
        let (start, end) = match axis {
            MoveAxis::Column => (source_rect.left, source_rect.right()),
            MoveAxis::Row => (source_rect.top, source_rect.bottom()),
        };
        if coordinate >= start - MOVE_SOURCE_SLOP_PX && coordinate <= end + MOVE_SOURCE_SLOP_PX {
            return Some(source);
        }
        let range = match axis {
            MoveAxis::Column => 0..self.grid.width(),
            MoveAxis::Row => 1..self.grid.height(),
        };
        range.min_by(|left, right| {
            let left_center = self.move_item_center(axis, *left).unwrap_or(f64::INFINITY);
            let right_center = self.move_item_center(axis, *right).unwrap_or(f64::INFINITY);
            (left_center - coordinate)
                .abs()
                .total_cmp(&(right_center - coordinate).abs())
        })
    }

    fn move_item_center(&self, axis: MoveAxis, index: usize) -> Option<f64> {
        let rect = self.move_item_rect(axis, index)?;
        match axis {
            MoveAxis::Column => Some(rect.left + rect.width / 2.0),
            MoveAxis::Row => Some(rect.top + rect.height / 2.0),
        }
    }

    fn move_item_rect(&self, axis: MoveAxis, index: usize) -> Option<HitRect> {
        let rect = match axis {
            MoveAxis::Column => {
                let header = row_element(&self.body, 0)?;
                cell_element(&header, index)?.get_bounding_client_rect()
            }
            MoveAxis::Row => row_element(&self.body, index)?.get_bounding_client_rect(),
        };
        Some(HitRect {
            left: rect.left(),
            top: rect.top(),
            width: rect.width(),
            height: rect.height(),
        })
    }

    fn release_pointer(&mut self, pointer_id: i32) {
        if let Some(capture) = self.pointer_capture.take()
            && capture.has_pointer_capture(pointer_id)
        {
            let _ = capture.release_pointer_capture(pointer_id);
        }
    }

    fn clear_move_marks(&mut self) -> Result<(), TableViewControllerError> {
        clear_marker(&self.table_selector, MOVE_SOURCE_ATTR)?;
        clear_marker(&self.table_selector, MOVE_TARGET_ATTR)?;
        let Some((axis, source, target)) = self.painted_move.take() else {
            return Ok(());
        };
        match axis {
            MoveAxis::Row => {
                if let Some(row) = row_element(&self.body, source) {
                    clear_marker(&row, MOVE_SOURCE_ATTR)?;
                }
                if let Some(row) = row_element(&self.body, target) {
                    clear_marker(&row, MOVE_TARGET_ATTR)?;
                }
                if let Some(button) = self.row_selectors.children().item(source as u32) {
                    clear_marker(&button, MOVE_SOURCE_ATTR)?;
                }
                if let Some(button) = self.row_selectors.children().item(target as u32) {
                    clear_marker(&button, MOVE_TARGET_ATTR)?;
                }
            }
            MoveAxis::Column => {
                for row in 0..self.grid.height() {
                    let Some(row_element) = row_element(&self.body, row) else {
                        continue;
                    };
                    if let Some(cell) = cell_element(&row_element, source) {
                        clear_marker(&cell, MOVE_SOURCE_ATTR)?;
                    }
                    if let Some(cell) = cell_element(&row_element, target) {
                        clear_marker(&cell, MOVE_TARGET_ATTR)?;
                    }
                }
                if let Some(button) = self.column_selectors.children().item(source as u32) {
                    clear_marker(&button, MOVE_SOURCE_ATTR)?;
                }
                if let Some(button) = self.column_selectors.children().item(target as u32) {
                    clear_marker(&button, MOVE_TARGET_ATTR)?;
                }
            }
        }
        Ok(())
    }

    fn clear_interaction_state(&mut self) -> Result<(), TableViewControllerError> {
        self.clear_move_marks()?;
        self.root
            .set_attribute(ROOT_STATE_ATTR, "ready")
            .map_err(dom_error)?;
        self.root
            .remove_attribute(ROOT_RESIZE_AXIS_ATTR)
            .map_err(dom_error)?;
        self.root
            .remove_attribute(ROOT_MOVE_AXIS_ATTR)
            .map(|_| ())
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

fn move_coordinate(axis: MoveAxis, event: &PointerEvent) -> f64 {
    match axis {
        MoveAxis::Column => event.client_x() as f64,
        MoveAxis::Row => event.client_y() as f64,
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

fn set_pixel_var(
    element: &Element,
    name: &str,
    value: f64,
) -> Result<(), TableViewControllerError> {
    if !value.is_finite() {
        return Ok(());
    }
    let html = element
        .clone()
        .dyn_into::<HtmlElement>()
        .map_err(|_| TableViewControllerError::Dom("table handle is not an HtmlElement".into()))?;
    html.style()
        .set_property(name, &format!("{value}px"))
        .map_err(dom_error)
}

fn set_marker(element: &Element, name: &str, value: &str) -> Result<(), TableViewControllerError> {
    element.set_attribute(name, value).map_err(dom_error)
}

fn clear_marker(element: &Element, name: &str) -> Result<(), TableViewControllerError> {
    element
        .remove_attribute(name)
        .map(|_| ())
        .map_err(dom_error)
}

fn set_selected(element: &Element, selected: bool) -> Result<(), TableViewControllerError> {
    element
        .set_attribute(TABLE_SELECTED_ATTR, if selected { "true" } else { "false" })
        .map_err(dom_error)
}

fn set_disabled(element: &Element, disabled: bool) -> Result<(), TableViewControllerError> {
    if disabled {
        element.set_attribute("disabled", "").map_err(dom_error)
    } else {
        element
            .remove_attribute("disabled")
            .map(|_| ())
            .map_err(dom_error)
    }
}

fn focus_element(element: &Element) -> Result<(), TableViewControllerError> {
    element
        .clone()
        .dyn_into::<HtmlElement>()
        .map_err(|_| TableViewControllerError::Dom("table action is not an HtmlElement".into()))?
        .focus()
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
