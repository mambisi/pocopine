use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use pine_icons::icon;
use pine_richtext::view::{
    NodeViewError, NodeViewHandle, NodeViewSpec, NodeViewUpdate, RichTextNodeView,
    RichTextViewExtension, use_node_view_handle,
};
use pocopine::prelude::*;
use pocopine::{Refs, current_scope_id};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::{MouseEvent, PointerEvent};

use super::super::{
    MoveColumn, MoveRow, ResizeColumn, ResizeRow, SelectCells, TableNode, TablesExtension,
};
use super::controller::{
    MoveAxis, ResizeAxis, TableViewAction, TableViewAnchor, TableViewDispatch,
    TableViewDispatchError,
};
use super::dom_controller::{TableViewController, TableViewControllerError, TableViewSnapshot};

thread_local! {
    static CONTROLLERS: RefCell<HashMap<ScopeId, TableViewController>> =
        RefCell::new(HashMap::new());
    static DISPATCHERS: RefCell<HashMap<ScopeId, Rc<dyn TableViewDispatch>>> =
        RefCell::new(HashMap::new());
    static PENDING_SNAPSHOTS: RefCell<HashMap<ScopeId, TableViewSnapshot>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableSelector {
    pub key: String,
    pub index: u64,
    pub label: String,
    pub short_label: String,
    #[serde(default)]
    pub draggable: bool,
}

/// Typed editable table shell.
///
/// The component owns toolbar/selector/resize chrome. Pine owns the semantic
/// row/cell descendants in the compile-time-proven `<tbody pp-owned-content>`
/// outlet, so component updates retain editable child identity.
#[derive(Serialize, Deserialize)]
#[component(
    template = "PineRichTextTable.poco",
    style = "table.css",
    role = "panel"
)]
pub struct PineRichTextTable {
    pub rows: Vec<TableSelector>,
    pub columns: Vec<TableSelector>,
    pub row_count: u64,
    pub column_count: u64,
    pub state: String,
    pub editable: bool,
    pub focused: bool,
    pub error: String,
    pub error_code: String,
}

impl Default for PineRichTextTable {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            columns: Vec::new(),
            row_count: 0,
            column_count: 0,
            state: "mounting".into(),
            editable: false,
            focused: false,
            error: String::new(),
            error_code: String::new(),
        }
    }
}

impl RichTextNodeView<TableNode> for PineRichTextTable {
    fn sync_node(
        &mut self,
        update: NodeViewUpdate<super::super::TableAttrs>,
    ) -> Result<(), NodeViewError> {
        let scope = current_scope_id().ok_or_else(|| NodeViewError::Context {
            requested: std::any::type_name::<TableNode>(),
            message: "table snapshot was delivered outside its component scope".into(),
        })?;
        let row_count = update.content.len();
        let column_count = update
            .content
            .child(0)
            .map_or(0, pine_richtext::model::Node::child_count);
        self.rows = selectors("row", row_count, update.editable);
        self.columns = selectors("column", column_count, update.editable);
        self.row_count = row_count as u64;
        self.column_count = column_count as u64;
        self.editable = update.editable;
        self.focused = update.editor_focused;
        self.state = "ready".into();
        self.error.clear();
        self.error_code.clear();

        let snapshot = TableViewSnapshot {
            attrs: update.attrs,
            content: update.content,
            selection: update.selection,
            editable: update.editable,
            focused: update.editor_focused,
        };
        PENDING_SNAPSHOTS.with(|pending| {
            pending.borrow_mut().insert(scope, snapshot.clone());
        });

        let anchor = DISPATCHERS.with(|dispatchers| {
            dispatchers
                .borrow()
                .get(&scope)
                .map(|dispatch| dispatch.live_anchor())
        });
        if let Some(anchor) = anchor {
            let anchor = anchor.map_err(node_view_dispatch_error)?;
            CONTROLLERS.with(|controllers| {
                if let Some(controller) = controllers.borrow_mut().get_mut(&scope) {
                    controller
                        .sync(anchor, snapshot)
                        .map_err(node_view_controller_error)?;
                }
                Ok::<(), NodeViewError>(())
            })?;
        }
        Ok(())
    }
}

impl RichTextViewExtension for TablesExtension {
    fn typed_node_views(&self) -> Vec<NodeViewSpec> {
        vec![NodeViewSpec::editable_component::<
            TableNode,
            PineRichTextTable,
        >()]
    }
}

#[handlers]
impl PineRichTextTable {
    #[computed]
    fn handle_icon() -> &'static str {
        icon!("grip-vertical")
    }

    fn on_ready(&self, refs: Refs, scope: ScopeId, handle: Handle<Self>) {
        if let Err(error) = self.attach_controller(&refs, scope) {
            handle.defer_update(|component| component.report_error("controller-attach", error));
        }
    }

    fn on_unmount(&mut self) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        CONTROLLERS.with(|controllers| {
            controllers.borrow_mut().remove(&scope);
        });
        DISPATCHERS.with(|dispatchers| {
            dispatchers.borrow_mut().remove(&scope);
        });
        PENDING_SNAPSHOTS.with(|pending| {
            pending.borrow_mut().remove(&scope);
        });
    }

    fn on_pointer_down(&mut self, event: wasm_bindgen::JsValue) {
        self.with_pointer_event(event, "pointer-down", true, |controller, event| {
            controller.pointer_down(event).map(|_| None)
        });
    }

    fn on_pointer_move(&mut self, event: wasm_bindgen::JsValue) {
        self.with_pointer_event(event, "pointer-move", false, |controller, event| {
            controller.pointer_move(event).map(|_| None)
        });
    }

    fn on_pointer_up(&mut self, event: wasm_bindgen::JsValue) {
        self.with_pointer_event(event, "pointer-up", false, |controller, event| {
            controller.pointer_up(event)
        });
    }

    fn on_pointer_cancel(&mut self, event: wasm_bindgen::JsValue) {
        self.with_pointer_event(event, "pointer-cancel", false, |controller, event| {
            controller.pointer_cancel(event).map(|_| None)
        });
    }

    fn on_viewport_scroll(&mut self) {
        self.refresh_handle_geometry("viewport-scroll");
    }

    fn on_table_resize(&mut self, _width: f64, _height: f64) {
        self.refresh_handle_geometry("table-resize");
    }

    fn dismiss_selection(&mut self) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let result = CONTROLLERS.with(|controllers| {
            controllers
                .borrow_mut()
                .get_mut(&scope)
                .map(TableViewController::dismiss_selection)
        });
        if let Some(Err(error)) = result {
            self.report_error("dismiss-selection", error.to_string());
        }
    }

    fn on_row_handle_pointer_down(&mut self, event: wasm_bindgen::JsValue, index: u64) {
        self.with_pointer_event(event, "row-move-start", true, |controller, event| {
            controller
                .pointer_down_move(event, MoveAxis::Row, index as usize)
                .map(|_| None)
        });
    }

    fn on_column_handle_pointer_down(&mut self, event: wasm_bindgen::JsValue, index: u64) {
        self.with_pointer_event(event, "column-move-start", true, |controller, event| {
            controller
                .pointer_down_move(event, MoveAxis::Column, index as usize)
                .map(|_| None)
        });
    }

    fn select_row(&mut self, event: MouseEvent, index: u64) {
        if event.detail() > 0 && self.consume_suppressed_click(MoveAxis::Row, index as usize) {
            event.prevent_default();
            return;
        }
        self.run_controller_action("select-row", |controller| {
            controller.restore_selection()?;
            controller.select_row(index as usize)
        });
    }

    fn select_column(&mut self, event: MouseEvent, index: u64) {
        if event.detail() > 0 && self.consume_suppressed_click(MoveAxis::Column, index as usize) {
            event.prevent_default();
            return;
        }
        self.run_controller_action("select-column", |controller| {
            controller.restore_selection()?;
            controller.select_column(index as usize)
        });
    }

    fn select_table(&mut self) {
        self.run_controller_action("select-table", |controller| {
            controller.restore_selection()?;
            Ok(controller.select_table())
        });
    }

    fn move_selection_backward(&mut self) {
        self.run_optional_controller_action("move-selection-backward", |controller| {
            Ok(controller.move_selected(false))
        });
        self.focus_reorder_action("move-selection-backward-focus", false);
    }

    fn move_selection_forward(&mut self) {
        self.run_optional_controller_action("move-selection-forward", |controller| {
            Ok(controller.move_selected(true))
        });
        self.focus_reorder_action("move-selection-forward-focus", true);
    }
}

impl PineRichTextTable {
    fn refresh_handle_geometry(&mut self, code: &'static str) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        let result = CONTROLLERS.with(|controllers| {
            controllers
                .borrow()
                .get(&scope)
                .map(TableViewController::refresh_handle_geometry)
        });
        let Some(result) = result else {
            return;
        };
        if let Err(error) = result {
            self.report_error(code, error.to_string());
        }
    }
    fn attach_controller(&self, refs: &Refs, scope: ScopeId) -> Result<(), String> {
        let handle = use_node_view_handle::<TableNode>().map_err(|error| error.to_string())?;
        let dispatch: Rc<dyn TableViewDispatch> = Rc::new(NodeHandleTableDispatch { handle });
        let anchor = dispatch.live_anchor().map_err(|error| error.to_string())?;
        let snapshot = PENDING_SNAPSHOTS
            .with(|pending| pending.borrow_mut().remove(&scope))
            .ok_or_else(|| "initial table snapshot is unavailable".to_string())?;
        let root = refs
            .get("root")
            .ok_or_else(|| "compiled table-view root ref is unavailable".to_string())?;
        let table = refs
            .get("table")
            .ok_or_else(|| "compiled table ref is unavailable".to_string())?;
        let body = refs
            .get("body")
            .ok_or_else(|| "compiled owned-content body ref is unavailable".to_string())?;
        let table_selector = refs
            .get("table_selector")
            .ok_or_else(|| "table selector ref is unavailable".to_string())?;
        let column_selectors = refs
            .get("column_selectors")
            .ok_or_else(|| "column selector ref is unavailable".to_string())?;
        let row_selectors = refs
            .get("row_selectors")
            .ok_or_else(|| "row selector ref is unavailable".to_string())?;
        let reorder_actions = refs
            .get("reorder_actions")
            .ok_or_else(|| "reorder actions ref is unavailable".to_string())?;
        let reorder_backward = refs
            .get("reorder_backward")
            .ok_or_else(|| "backward reorder ref is unavailable".to_string())?;
        let reorder_forward = refs
            .get("reorder_forward")
            .ok_or_else(|| "forward reorder ref is unavailable".to_string())?;
        let controller = TableViewController::attach(
            anchor,
            root,
            table,
            body,
            table_selector,
            column_selectors,
            row_selectors,
            reorder_actions,
            reorder_backward,
            reorder_forward,
            snapshot,
        )
        .map_err(|error| error.to_string())?;
        DISPATCHERS.with(|dispatchers| {
            dispatchers.borrow_mut().insert(scope, dispatch);
        });
        CONTROLLERS.with(|controllers| {
            controllers.borrow_mut().insert(scope, controller);
        });
        Ok(())
    }

    fn with_pointer_event(
        &mut self,
        event: wasm_bindgen::JsValue,
        code: &'static str,
        refresh_anchor: bool,
        run: impl FnOnce(
            &mut TableViewController,
            PointerEvent,
        ) -> Result<Option<TableViewAction>, TableViewControllerError>,
    ) {
        let Some(scope) = current_scope_id() else {
            self.report_error(code, "component scope is unavailable".into());
            return;
        };
        let Ok(event) = event.dyn_into::<PointerEvent>() else {
            self.report_error(code, "handler received a non-pointer event".into());
            return;
        };
        let live_anchor = if refresh_anchor {
            match self.live_anchor(scope) {
                Ok(anchor) => Some(anchor),
                Err(error) => {
                    self.report_error(code, error.to_string());
                    return;
                }
            }
        } else {
            None
        };
        // End the controller RefCell borrow before dispatch. A dispatch may
        // synchronously sync this same retained component at Pocopine's safe
        // point; holding the map borrow across it would be reentrant.
        let action = CONTROLLERS.with(|controllers| {
            let mut controllers = controllers.borrow_mut();
            let controller = controllers.get_mut(&scope).ok_or_else(|| {
                TableViewControllerError::Dom("table controller is unavailable".into())
            })?;
            if let Some(anchor) = live_anchor {
                controller.refresh_anchor(anchor);
            }
            run(controller, event)
        });
        match action {
            Ok(Some(action)) => self.dispatch_action(scope, code, action),
            Ok(None) => {}
            Err(error) => self.report_error(code, error.to_string()),
        }
    }

    fn run_controller_action(
        &mut self,
        code: &'static str,
        build: impl FnOnce(
            &mut TableViewController,
        ) -> Result<TableViewAction, TableViewControllerError>,
    ) {
        let Some(scope) = current_scope_id() else {
            self.report_error(code, "component scope is unavailable".into());
            return;
        };
        let live_anchor = match self.live_anchor(scope) {
            Ok(anchor) => anchor,
            Err(error) => {
                self.report_error(code, error.to_string());
                return;
            }
        };
        let action = CONTROLLERS.with(|controllers| {
            let mut controllers = controllers.borrow_mut();
            let controller = controllers.get_mut(&scope).ok_or_else(|| {
                TableViewControllerError::Dom("table controller is unavailable".into())
            })?;
            controller.refresh_anchor(live_anchor);
            build(controller)
        });
        match action {
            Ok(action) => self.dispatch_action(scope, code, action),
            Err(error) => self.report_error(code, error.to_string()),
        }
    }

    fn run_optional_controller_action(
        &mut self,
        code: &'static str,
        build: impl FnOnce(
            &TableViewController,
        ) -> Result<Option<TableViewAction>, TableViewControllerError>,
    ) {
        let Some(scope) = current_scope_id() else {
            self.report_error(code, "component scope is unavailable".into());
            return;
        };
        let live_anchor = match self.live_anchor(scope) {
            Ok(anchor) => anchor,
            Err(error) => {
                self.report_error(code, error.to_string());
                return;
            }
        };
        let action = CONTROLLERS.with(|controllers| {
            let mut controllers = controllers.borrow_mut();
            let controller = controllers.get_mut(&scope).ok_or_else(|| {
                TableViewControllerError::Dom("table controller is unavailable".into())
            })?;
            controller.refresh_anchor(live_anchor);
            build(controller)
        });
        match action {
            Ok(Some(action)) => self.dispatch_action(scope, code, action),
            Ok(None) => {}
            Err(error) => self.report_error(code, error.to_string()),
        }
    }

    fn dispatch_action(&mut self, scope: ScopeId, code: &'static str, action: TableViewAction) {
        let dispatch = DISPATCHERS.with(|dispatchers| dispatchers.borrow().get(&scope).cloned());
        let Some(dispatch) = dispatch else {
            self.report_error(code, "typed table dispatcher is unavailable".into());
            return;
        };
        if let Err(error) = dispatch.dispatch(action) {
            self.report_error(code, error.to_string());
        }
    }

    fn live_anchor(&self, scope: ScopeId) -> Result<TableViewAnchor, TableViewDispatchError> {
        DISPATCHERS
            .with(|dispatchers| dispatchers.borrow().get(&scope).cloned())
            .ok_or_else(|| {
                TableViewDispatchError::Editor("typed table dispatcher is unavailable".into())
            })?
            .live_anchor()
    }

    fn focus_reorder_action(&self, code: &'static str, prefer_forward: bool) {
        let Some(scope) = current_scope_id() else {
            return;
        };
        // Native click/keyboard activation can apply its own focus step after
        // this handler returns. Run after the reactive flush so our deliberate
        // boundary fallback is the final focus destination.
        pocopine::tick::after_flush(move || {
            let result = CONTROLLERS.with(|controllers| {
                controllers
                    .borrow()
                    .get(&scope)
                    .map(|controller| controller.focus_reorder_action(prefer_forward))
            });
            if let Some(Err(error)) = result {
                tracing::warn!(target: "pocopine.log", %error, code, "table reorder focus failed");
            }
        });
    }

    fn consume_suppressed_click(&self, axis: MoveAxis, index: usize) -> bool {
        let Some(scope) = current_scope_id() else {
            return false;
        };
        CONTROLLERS.with(|controllers| {
            controllers
                .borrow_mut()
                .get_mut(&scope)
                .is_some_and(|controller| controller.consume_suppressed_click(axis, index))
        })
    }

    fn report_error(&mut self, code: impl Into<String>, error: String) {
        self.state = "error".into();
        self.error_code = code.into();
        self.error = error.clone();
        tracing::warn!(target: "pocopine.log", %error, "rich-text table view failed");
    }
}

struct NodeHandleTableDispatch {
    /// The handle itself is the unforgeable host-generation token.
    handle: NodeViewHandle<TableNode>,
}

impl TableViewDispatch for NodeHandleTableDispatch {
    fn live_anchor(&self) -> Result<TableViewAnchor, TableViewDispatchError> {
        self.handle
            .position()
            .map(|table_pos| TableViewAnchor { table_pos })
            .map_err(|error| TableViewDispatchError::Editor(error.to_string()))
    }

    fn dispatch(&self, action: TableViewAction) -> Result<(), TableViewDispatchError> {
        let expected = match action {
            TableViewAction::Resize(commit) => commit.anchor,
            TableViewAction::Select(commit) => commit.anchor,
            TableViewAction::Move(commit) => commit.anchor,
        };
        let actual = self.live_anchor().ok();
        if actual != Some(expected) {
            return Err(TableViewDispatchError::Stale { expected, actual });
        }
        let result = match action {
            TableViewAction::Resize(commit) => match commit.axis {
                ResizeAxis::Column => self.handle.dispatch(ResizeColumn {
                    expected_table_pos: commit.anchor.table_pos,
                    column: commit.index,
                    width: commit.size,
                }),
                ResizeAxis::Row => self.handle.dispatch(ResizeRow {
                    expected_table_pos: commit.anchor.table_pos,
                    row: commit.index,
                    height: commit.size,
                }),
            },
            TableViewAction::Select(commit) => self.handle.dispatch(SelectCells {
                expected_table_pos: commit.anchor.table_pos,
                anchor_row: commit.anchor_row,
                anchor_column: commit.anchor_column,
                head_row: commit.head_row,
                head_column: commit.head_column,
            }),
            TableViewAction::Move(commit) => match commit.axis {
                MoveAxis::Column => self.handle.dispatch(MoveColumn {
                    expected_table_pos: commit.anchor.table_pos,
                    source: commit.source,
                    target: commit.target,
                }),
                MoveAxis::Row => self.handle.dispatch(MoveRow {
                    expected_table_pos: commit.anchor.table_pos,
                    source: commit.source,
                    target: commit.target,
                }),
            },
        };
        result.map_err(|error| TableViewDispatchError::Editor(error.to_string()))
    }
}

fn selectors(kind: &str, count: usize, editable: bool) -> Vec<TableSelector> {
    let has_move_destination = if kind == "row" { count > 2 } else { count > 1 };
    (0..count)
        .map(|index| {
            let human = index + 1;
            let can_move = editable && has_move_destination && (kind != "row" || index > 0);
            TableSelector {
                key: format!("{kind}-{index}"),
                index: index as u64,
                label: match (kind, index, can_move) {
                    ("row", 0, _) => "Select header row".into(),
                    ("row", _, false) => format!("Select row {human}"),
                    (_, _, false) => format!("Select column {}", column_label(index)),
                    ("row", _, true) => {
                        format!("Row {human}: select for move buttons, or drag to reorder")
                    }
                    (_, _, true) => format!(
                        "Column {}: select for move buttons, or drag to reorder",
                        column_label(index)
                    ),
                },
                short_label: if kind == "row" {
                    human.to_string()
                } else {
                    column_label(index)
                },
                draggable: can_move,
            }
        })
        .collect()
}

fn column_label(mut index: usize) -> String {
    let mut chars = Vec::new();
    loop {
        chars.push((b'A' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    chars.into_iter().rev().collect()
}

fn node_view_dispatch_error(error: TableViewDispatchError) -> NodeViewError {
    NodeViewError::Dispatch {
        node_type: "table".into(),
        message: error.to_string(),
    }
}

fn node_view_controller_error(error: TableViewControllerError) -> NodeViewError {
    NodeViewError::Sync {
        component: "pine-rich-text-table",
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spreadsheet_column_labels_are_stable() {
        assert_eq!(column_label(0), "A");
        assert_eq!(column_label(25), "Z");
        assert_eq!(column_label(26), "AA");
        assert_eq!(column_label(51), "AZ");
        assert_eq!(column_label(52), "BA");
    }

    #[test]
    fn selectors_only_offer_dragging_when_a_destination_exists() {
        assert!(!selectors("column", 1, true)[0].draggable);
        assert!(
            selectors("column", 2, true)
                .iter()
                .all(|selector| selector.draggable)
        );

        let minimal_rows = selectors("row", 2, true);
        assert!(minimal_rows.iter().all(|selector| !selector.draggable));
        let movable_rows = selectors("row", 3, true);
        assert!(!movable_rows[0].draggable);
        assert!(movable_rows[1..].iter().all(|selector| selector.draggable));

        let read_only = selectors("column", 2, false);
        assert!(read_only.iter().all(|selector| !selector.draggable));
        assert_eq!(read_only[0].label, "Select column A");
    }
}
