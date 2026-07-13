use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use pine_richtext::view::{
    NodeViewError, NodeViewHandle, NodeViewSpec, NodeViewUpdate, RichTextNodeView,
    RichTextViewExtension, use_node_view_handle,
};
use pocopine::prelude::*;
use pocopine::{Refs, current_scope_id};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use web_sys::PointerEvent;

use super::super::{ResizeColumn, ResizeRow, SelectCells, TableNode, TablesExtension};
use super::controller::{
    ResizeAxis, TableViewAction, TableViewAnchor, TableViewDispatch, TableViewDispatchError,
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
        self.rows = selectors("row", row_count);
        self.columns = selectors("column", column_count);
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
        self.with_pointer_event(event, "pointer-down", |controller, event| {
            controller.pointer_down(event).map(|_| None)
        });
    }

    fn on_pointer_move(&mut self, event: wasm_bindgen::JsValue) {
        self.with_pointer_event(event, "pointer-move", |controller, event| {
            controller.pointer_move(event).map(|_| None)
        });
    }

    fn on_pointer_up(&mut self, event: wasm_bindgen::JsValue) {
        self.with_pointer_event(event, "pointer-up", |controller, event| {
            controller.pointer_up(event)
        });
    }

    fn on_pointer_cancel(&mut self, event: wasm_bindgen::JsValue) {
        self.with_pointer_event(event, "pointer-cancel", |controller, event| {
            controller.pointer_cancel(event).map(|_| None)
        });
    }

    fn select_row(&mut self, index: u64) {
        self.run_controller_action("select-row", |controller| {
            controller.select_row(index as usize)
        });
    }

    fn select_column(&mut self, index: u64) {
        self.run_controller_action("select-column", |controller| {
            controller.select_column(index as usize)
        });
    }

    fn select_table(&mut self) {
        self.run_controller_action("select-table", |controller| Ok(controller.select_table()));
    }
}

impl PineRichTextTable {
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
        let controller = TableViewController::attach(
            anchor,
            root,
            table,
            body,
            table_selector,
            column_selectors,
            row_selectors,
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
        // End the controller RefCell borrow before dispatch. A dispatch may
        // synchronously sync this same retained component at Pocopine's safe
        // point; holding the map borrow across it would be reentrant.
        let action = CONTROLLERS.with(|controllers| {
            let mut controllers = controllers.borrow_mut();
            let controller = controllers.get_mut(&scope).ok_or_else(|| {
                TableViewControllerError::Dom("table controller is unavailable".into())
            })?;
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
        build: impl FnOnce(&TableViewController) -> Result<TableViewAction, TableViewControllerError>,
    ) {
        let Some(scope) = current_scope_id() else {
            self.report_error(code, "component scope is unavailable".into());
            return;
        };
        let action = CONTROLLERS.with(|controllers| {
            let controllers = controllers.borrow();
            let controller = controllers.get(&scope).ok_or_else(|| {
                TableViewControllerError::Dom("table controller is unavailable".into())
            })?;
            build(controller)
        });
        match action {
            Ok(action) => self.dispatch_action(scope, code, action),
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
        };
        result.map_err(|error| TableViewDispatchError::Editor(error.to_string()))
    }
}

fn selectors(kind: &str, count: usize) -> Vec<TableSelector> {
    (0..count)
        .map(|index| {
            let human = index + 1;
            TableSelector {
                key: format!("{kind}-{index}"),
                index: index as u64,
                label: format!("Select {kind} {human}"),
                short_label: if kind == "row" {
                    human.to_string()
                } else {
                    column_label(index)
                },
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
}
