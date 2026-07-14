//! Typed editable table view, native row/cell hosts, and interaction control.

mod component;
mod controller;
mod dom_controller;
mod grid;

pub use super::{
    TABLE_CELL_ATTR, TABLE_CELL_CLASS, TABLE_HEADER_CELL_CLASS, TABLE_ROW_ATTR, TABLE_ROW_CLASS,
    TABLE_SELECTED_ATTR,
};
pub use component::PineRichTextTable;
pub use controller::{
    CellSelectionCommit, HitRect, MoveAxis, MoveCommit, MoveDrag, ResizeAxis, ResizeCommit,
    ResizeDrag, ResizeEdge, TableViewAction, TableViewAnchor, TableViewDispatch,
    TableViewDispatchError, hit_test_resize_edge,
};
pub use dom_controller::{TableViewController, TableViewControllerError, TableViewSnapshot};
pub use grid::{TableViewGrid, TableViewGridError, ViewCell, ViewSelectionRect};
