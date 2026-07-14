//! Browser-independent state machines for table selection, resizing, and
//! reordering.
//!
//! Pointer handlers translate DOM coordinates into these types. The state
//! machines deliberately return semantic actions instead of dispatching while
//! a pointer is moving: resize previews stay local to the component and one
//! action is committed on pointer-up.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::super::{MAX_COLUMN_WIDTH, MAX_ROW_HEIGHT, MIN_COLUMN_WIDTH, MIN_ROW_HEIGHT};

/// Start position of the live table view that began an interaction.
///
/// The dispatcher itself owns a cloned `NodeViewHandle<TableNode>` as the
/// opaque generation token. Its private host generation is revalidated on
/// dispatch; this public value exists for diagnostics and rejects a table that
/// moved during a gesture without exposing or forging manager identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableViewAnchor {
    pub table_pos: usize,
}

/// Axis controlled by a resize gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResizeAxis {
    Column,
    Row,
}

/// Axis controlled by a row or column reorder gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveAxis {
    Column,
    Row,
}

/// One semantic resize committed after a completed pointer gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeCommit {
    pub anchor: TableViewAnchor,
    pub axis: ResizeAxis,
    pub index: usize,
    pub size: u32,
}

/// A rectangular cell selection action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellSelectionCommit {
    pub anchor: TableViewAnchor,
    pub anchor_row: usize,
    pub anchor_column: usize,
    pub head_row: usize,
    pub head_column: usize,
}

/// One semantic move committed after a completed handle drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MoveCommit {
    pub anchor: TableViewAnchor,
    pub axis: MoveAxis,
    pub source: usize,
    pub target: usize,
}

/// Semantic action produced by the table view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableViewAction {
    Resize(ResizeCommit),
    Select(CellSelectionCommit),
    Move(MoveCommit),
}

/// Narrow adapter between the extension-owned controller and Pine's typed
/// node-view handle.
///
/// The concrete implementation is expected to hold
/// `NodeViewHandle<TableNode>`. Keeping the boundary here makes the resize
/// algorithm testable on the host and prevents a DOM event-string command
/// bridge from becoming part of the editor contract.
pub trait TableViewDispatch {
    fn live_anchor(&self) -> Result<TableViewAnchor, TableViewDispatchError>;

    fn dispatch(&self, action: TableViewAction) -> Result<(), TableViewDispatchError>;
}

/// Recoverable failure while resolving or dispatching a table interaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableViewDispatchError {
    Stale {
        expected: TableViewAnchor,
        actual: Option<TableViewAnchor>,
    },
    Editor(String),
}

impl fmt::Display for TableViewDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stale { expected, actual } => write!(
                formatter,
                "table interaction started at position {}, but the live binding is {actual:?}",
                expected.table_pos
            ),
            Self::Editor(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for TableViewDispatchError {}

/// Pixel rectangle used for edge hit-testing without depending on `web-sys`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitRect {
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
}

impl HitRect {
    pub fn right(self) -> f64 {
        self.left + self.width
    }

    pub fn bottom(self) -> f64 {
        self.top + self.height
    }
}

/// Which edge of a cell accepted a resize pointer-down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResizeEdge {
    pub axis: ResizeAxis,
    pub index: usize,
}

/// Pick the closest supported edge inside `threshold` CSS pixels.
///
/// The right edge controls the cell's logical column and the bottom edge
/// controls its row. At the bottom-right corner the physically closest edge
/// wins; equal distances prefer columns so the result is deterministic.
pub fn hit_test_resize_edge(
    rect: HitRect,
    client_x: f64,
    client_y: f64,
    row: usize,
    column: usize,
    threshold: f64,
) -> Option<ResizeEdge> {
    if !threshold.is_finite()
        || threshold < 0.0
        || !client_x.is_finite()
        || !client_y.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return None;
    }
    let within_x = client_x >= rect.left - threshold && client_x <= rect.right() + threshold;
    let within_y = client_y >= rect.top - threshold && client_y <= rect.bottom() + threshold;
    if !within_x || !within_y {
        return None;
    }

    let column_distance = (rect.right() - client_x).abs();
    let row_distance = (rect.bottom() - client_y).abs();
    match (column_distance <= threshold, row_distance <= threshold) {
        (true, true) if column_distance <= row_distance => Some(ResizeEdge {
            axis: ResizeAxis::Column,
            index: column,
        }),
        (true, true) => Some(ResizeEdge {
            axis: ResizeAxis::Row,
            index: row,
        }),
        (true, false) => Some(ResizeEdge {
            axis: ResizeAxis::Column,
            index: column,
        }),
        (false, true) => Some(ResizeEdge {
            axis: ResizeAxis::Row,
            index: row,
        }),
        (false, false) => None,
    }
}

/// Active drag. This owns preview math only and has no editor access.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeDrag {
    anchor: TableViewAnchor,
    pointer_id: i32,
    edge: ResizeEdge,
    start_coordinate: f64,
    start_size: u32,
    preview_size: u32,
}

impl ResizeDrag {
    pub fn begin(
        anchor: TableViewAnchor,
        pointer_id: i32,
        edge: ResizeEdge,
        start_coordinate: f64,
        start_size: f64,
    ) -> Option<Self> {
        if !start_coordinate.is_finite() || !start_size.is_finite() || start_size <= 0.0 {
            return None;
        }
        let start_size = clamp_size(edge.axis, start_size.round() as i64);
        Some(Self {
            anchor,
            pointer_id,
            edge,
            start_coordinate,
            start_size,
            preview_size: start_size,
        })
    }

    pub fn pointer_id(self) -> i32 {
        self.pointer_id
    }

    pub fn edge(self) -> ResizeEdge {
        self.edge
    }

    pub fn preview_size(self) -> u32 {
        self.preview_size
    }

    /// Update the local preview. Events from another pointer are ignored.
    pub fn update(&mut self, pointer_id: i32, coordinate: f64) -> Option<u32> {
        if pointer_id != self.pointer_id || !coordinate.is_finite() {
            return None;
        }
        let delta = coordinate - self.start_coordinate;
        let candidate = (self.start_size as f64 + delta).round();
        self.preview_size = clamp_size(self.edge.axis, candidate as i64);
        Some(self.preview_size)
    }

    /// Finish the gesture. Unchanged geometry does not create a history item.
    pub fn finish(self, pointer_id: i32) -> Option<ResizeCommit> {
        (pointer_id == self.pointer_id && self.preview_size != self.start_size).then_some(
            ResizeCommit {
                anchor: self.anchor,
                axis: self.edge.axis,
                index: self.edge.index,
                size: self.preview_size,
            },
        )
    }
}

fn clamp_size(axis: ResizeAxis, size: i64) -> u32 {
    let (minimum, maximum) = match axis {
        ResizeAxis::Column => (MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH),
        ResizeAxis::Row => (MIN_ROW_HEIGHT, MAX_ROW_HEIGHT),
    };
    size.clamp(minimum as i64, maximum as i64) as u32
}

/// Local preview for a row or column handle drag.
///
/// A small movement threshold keeps an ordinary handle click a selection
/// action. Once crossed, pointer-up is consumed even if the pointer returns to
/// the source, but only a changed target produces an editor transaction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveDrag {
    anchor: TableViewAnchor,
    pointer_id: i32,
    axis: MoveAxis,
    source: usize,
    target: usize,
    start_coordinate: f64,
    active: bool,
}

impl MoveDrag {
    pub const ACTIVATION_DISTANCE_PX: f64 = 4.0;

    pub fn begin(
        anchor: TableViewAnchor,
        pointer_id: i32,
        axis: MoveAxis,
        source: usize,
        start_coordinate: f64,
    ) -> Option<Self> {
        if !start_coordinate.is_finite() || (axis == MoveAxis::Row && source == 0) {
            return None;
        }
        Some(Self {
            anchor,
            pointer_id,
            axis,
            source,
            target: source,
            start_coordinate,
            active: false,
        })
    }

    pub fn pointer_id(self) -> i32 {
        self.pointer_id
    }

    pub fn axis(self) -> MoveAxis {
        self.axis
    }

    pub fn source(self) -> usize {
        self.source
    }

    pub fn target(self) -> usize {
        self.target
    }

    pub fn is_active(self) -> bool {
        self.active
    }

    /// Update the preview target. Events from another pointer are ignored.
    pub fn update(&mut self, pointer_id: i32, coordinate: f64, target: usize) -> bool {
        if pointer_id != self.pointer_id || !coordinate.is_finite() {
            return false;
        }
        if (coordinate - self.start_coordinate).abs() >= Self::ACTIVATION_DISTANCE_PX {
            self.active = true;
        }
        self.target = if self.axis == MoveAxis::Row {
            target.max(1)
        } else {
            target
        };
        true
    }

    /// Finish the gesture. A click or a return to the source is transaction-free.
    pub fn finish(self, pointer_id: i32) -> Option<MoveCommit> {
        (pointer_id == self.pointer_id && self.active && self.target != self.source).then_some(
            MoveCommit {
                anchor: self.anchor,
                axis: self.axis,
                source: self.source,
                target: self.target,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANCHOR: TableViewAnchor = TableViewAnchor { table_pos: 8 };

    #[test]
    fn closest_edge_wins_at_cell_corner() {
        let rect = HitRect {
            left: 10.0,
            top: 20.0,
            width: 100.0,
            height: 40.0,
        };
        assert_eq!(
            hit_test_resize_edge(rect, 109.0, 55.0, 2, 3, 6.0),
            Some(ResizeEdge {
                axis: ResizeAxis::Column,
                index: 3,
            })
        );
        assert_eq!(
            hit_test_resize_edge(rect, 105.0, 59.0, 2, 3, 6.0),
            Some(ResizeEdge {
                axis: ResizeAxis::Row,
                index: 2,
            })
        );
        assert_eq!(hit_test_resize_edge(rect, 50.0, 40.0, 2, 3, 6.0), None);
    }

    #[test]
    fn resize_clamps_and_finishes_once() {
        let mut drag = ResizeDrag::begin(
            ANCHOR,
            4,
            ResizeEdge {
                axis: ResizeAxis::Column,
                index: 1,
            },
            100.0,
            80.0,
        )
        .unwrap();
        assert_eq!(drag.update(99, 500.0), None);
        assert_eq!(drag.update(4, -500.0), Some(MIN_COLUMN_WIDTH));
        assert_eq!(
            drag.finish(4),
            Some(ResizeCommit {
                anchor: ANCHOR,
                axis: ResizeAxis::Column,
                index: 1,
                size: MIN_COLUMN_WIDTH,
            })
        );
    }

    #[test]
    fn cancel_is_represented_by_dropping_drag_and_unchanged_finish_is_none() {
        let drag = ResizeDrag::begin(
            ANCHOR,
            7,
            ResizeEdge {
                axis: ResizeAxis::Row,
                index: 0,
            },
            40.0,
            30.0,
        )
        .unwrap();
        assert_eq!(drag.finish(7), None);
    }

    #[test]
    fn move_drag_distinguishes_click_from_one_pointer_up_commit() {
        let mut drag = MoveDrag::begin(ANCHOR, 11, MoveAxis::Column, 0, 20.0).unwrap();
        assert!(drag.update(11, 22.0, 1));
        assert!(!drag.is_active());
        assert_eq!(drag.finish(11), None);

        let mut drag = MoveDrag::begin(ANCHOR, 12, MoveAxis::Column, 0, 20.0).unwrap();
        assert!(!drag.update(99, 80.0, 2));
        assert!(drag.update(12, 80.0, 2));
        assert_eq!(
            drag.finish(12),
            Some(MoveCommit {
                anchor: ANCHOR,
                axis: MoveAxis::Column,
                source: 0,
                target: 2,
            })
        );
    }

    #[test]
    fn header_row_is_not_a_move_source_or_target() {
        assert!(MoveDrag::begin(ANCHOR, 13, MoveAxis::Row, 0, 20.0).is_none());
        let mut drag = MoveDrag::begin(ANCHOR, 13, MoveAxis::Row, 2, 20.0).unwrap();
        assert!(drag.update(13, 0.0, 0));
        assert_eq!(drag.target(), 1);
        assert_eq!(
            drag.finish(13),
            Some(MoveCommit {
                anchor: ANCHOR,
                axis: MoveAxis::Row,
                source: 2,
                target: 1,
            })
        );
    }
}
