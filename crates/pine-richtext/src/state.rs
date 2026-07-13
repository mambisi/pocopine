use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{Attrs, Fragment, Mark, Node, Schema, Slice};
use crate::transform::{Mapping, StepMap, Transform};
use crate::typed_nodes::WireNode;
use crate::{RichTextError, RichTextResult};

mod cell_selection;

pub use cell_selection::CellSelectionRect;

/// Meta key set on transactions that originated from a plugin's
/// `append_transaction` hook. Matches upstream's `appendedTransaction` key.
pub const META_APPENDED_TRANSACTION: &str = "appendedTransaction";

/// A selection in the editor document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Selection {
    /// Text selection with anchor/head positions.
    Text { anchor: usize, head: usize },
    /// Node selection anchored at the position before a node.
    Node { anchor: usize },
    /// Rectangular selection between two semantic table cells.
    ///
    /// Both positions point immediately before a node whose schema declares
    /// [`crate::model::TableRole::Cell`]. The cells must belong to the same
    /// rectangular semantic table. The rectangle is derived from the table
    /// structure rather than represented as a misleading linear text range.
    Cells {
        anchor_cell: usize,
        head_cell: usize,
    },
    /// Selection covering the full document.
    All,
}

impl Selection {
    /// Collapsed text selection.
    pub fn text(pos: usize) -> Self {
        Self::Text {
            anchor: pos,
            head: pos,
        }
    }

    /// Text selection with explicit anchor/head.
    pub fn text_between(anchor: usize, head: usize) -> Self {
        Self::Text { anchor, head }
    }

    /// Find a valid cursor or node selection from a document position.
    pub fn find_from(
        doc: &Node,
        schema: &Schema,
        pos: usize,
        dir: i8,
        text_only: bool,
    ) -> RichTextResult<Option<Self>> {
        let dir = normalize_dir(dir);
        let resolved = doc.resolve(pos)?;
        if node_has_inline_content(schema, resolved.parent()) {
            return Ok(Some(Self::text(pos)));
        }

        if let Some(found) = find_selection_in(
            schema,
            resolved.parent(),
            pos,
            resolved.index(resolved.depth()).unwrap_or(0),
            dir,
            text_only,
        ) {
            return Ok(Some(found));
        }

        for depth in (0..resolved.depth()).rev() {
            let Some(node) = resolved.node(depth) else {
                continue;
            };
            let found = if dir < 0 {
                resolved.before(depth + 1).and_then(|before| {
                    find_selection_in(
                        schema,
                        node,
                        before,
                        resolved.index(depth).unwrap_or(0),
                        dir,
                        text_only,
                    )
                })
            } else {
                resolved.after(depth + 1).and_then(|after| {
                    find_selection_in(
                        schema,
                        node,
                        after,
                        resolved.index(depth).unwrap_or(0) + 1,
                        dir,
                        text_only,
                    )
                })
            };
            if found.is_some() {
                return Ok(found);
            }
        }

        Ok(None)
    }

    /// Find the nearest valid cursor or node selection around a document position.
    pub fn near(doc: &Node, schema: &Schema, pos: usize, bias: i8) -> RichTextResult<Self> {
        let bias = normalize_dir(bias);
        if let Some(selection) = Self::find_from(doc, schema, pos, bias, false)? {
            return Ok(selection);
        }
        if let Some(selection) = Self::find_from(doc, schema, pos, -bias, false)? {
            return Ok(selection);
        }
        Ok(Self::All)
    }

    /// Find the closest valid selection to the start of a document.
    pub fn at_start(doc: &Node, schema: &Schema) -> Self {
        find_selection_in(schema, doc, 0, 0, 1, false).unwrap_or(Self::All)
    }

    /// Find the closest valid selection to the end of a document.
    pub fn at_end(doc: &Node, schema: &Schema) -> Self {
        find_selection_in(
            schema,
            doc,
            doc.content_size(),
            doc.child_count(),
            -1,
            false,
        )
        .unwrap_or(Self::All)
    }

    /// Create a text selection, adjusting endpoints to nearby text positions when needed.
    pub fn text_between_near(
        doc: &Node,
        schema: &Schema,
        anchor: usize,
        head: usize,
        bias: Option<i8>,
    ) -> RichTextResult<Self> {
        let distance = anchor as isize - head as isize;
        let bias = match (bias, distance) {
            (_, distance) if distance > 0 => 1,
            (_, distance) if distance < 0 => -1,
            (Some(bias), _) => normalize_dir(bias),
            (None, _) => 1,
        };

        let mut head_pos = head;
        if !is_text_position(doc, schema, head)? {
            let found = match Self::find_from(doc, schema, head, bias, true)? {
                Some(selection) => Some(selection),
                None => Self::find_from(doc, schema, head, -bias, true)?,
            };
            if let Some(Self::Text { head, .. }) = found {
                head_pos = head;
            } else {
                return Self::near(doc, schema, head, bias);
            }
        }

        let mut anchor_pos = anchor;
        if !is_text_position(doc, schema, anchor)? {
            if distance == 0 {
                anchor_pos = head_pos;
            } else {
                let found = match Self::find_from(doc, schema, anchor, -bias, true)? {
                    Some(selection) => Some(selection),
                    None => Self::find_from(doc, schema, anchor, bias, true)?,
                };
                if let Some(Self::Text { anchor, .. }) = found {
                    anchor_pos = if (anchor < head_pos) != (distance < 0) {
                        head_pos
                    } else {
                        anchor
                    };
                } else {
                    anchor_pos = head_pos;
                }
            }
        }

        Ok(Self::Text {
            anchor: anchor_pos,
            head: head_pos,
        })
    }

    /// Node selection.
    pub fn node(anchor: usize) -> Self {
        Self::Node { anchor }
    }

    /// Rectangular semantic table-cell selection.
    pub fn cells(anchor_cell: usize, head_cell: usize) -> Self {
        Self::Cells {
            anchor_cell,
            head_cell,
        }
    }

    /// Whether this is a rectangular table-cell selection.
    pub fn is_cells(&self) -> bool {
        matches!(self, Self::Cells { .. })
    }

    /// Selection start.
    pub fn from(&self, doc: &Node) -> usize {
        match self {
            Self::Text { anchor, head } => (*anchor).min(*head),
            Self::Node { anchor } => *anchor,
            Self::Cells {
                anchor_cell,
                head_cell,
            } => cell_selection::structural_bounds(doc, *anchor_cell, *head_cell)
                .map(|(from, _)| from)
                .unwrap_or((*anchor_cell).min(*head_cell)),
            Self::All => 0,
        }
        .min(doc.content_size())
    }

    /// Selection end.
    pub fn to(&self, doc: &Node) -> usize {
        match self {
            Self::Text { anchor, head } => (*anchor).max(*head),
            Self::Node { anchor } => node_after(doc, *anchor)
                .map(|node| anchor + node.node_size())
                .unwrap_or(*anchor)
                .min(doc.content_size()),
            Self::Cells {
                anchor_cell,
                head_cell,
            } => cell_selection::structural_bounds(doc, *anchor_cell, *head_cell)
                .map(|(_, to)| to)
                .unwrap_or((*anchor_cell).max(*head_cell))
                .min(doc.content_size()),
            Self::All => doc.content_size(),
        }
    }

    /// Whether this selection is empty.
    pub fn is_empty(&self, doc: &Node) -> bool {
        !self.is_cells() && self.from(doc) == self.to(doc)
    }

    /// Map this selection through a transform mapping.
    pub fn map(&self, mapping: &Mapping) -> Self {
        self.map_through_maps(&mapping.maps)
    }

    fn map_through_maps(&self, maps: &[StepMap]) -> Self {
        match self {
            Self::Text { anchor, head } => Self::Text {
                anchor: Mapping::map_through_maps(maps, *anchor, 1),
                head: Mapping::map_through_maps(maps, *head, 1),
            },
            Self::Node { anchor } => Self::Node {
                anchor: Mapping::map_through_maps(maps, *anchor, 1),
            },
            Self::Cells {
                anchor_cell,
                head_cell,
            } => Self::Cells {
                anchor_cell: Mapping::map_through_maps(maps, *anchor_cell, 1),
                head_cell: Mapping::map_through_maps(maps, *head_cell, 1),
            },
            Self::All => Self::All,
        }
    }

    /// Clamp this selection to a document's current content range.
    pub fn clamped(&self, doc: &Node) -> Self {
        let size = doc.content_size();
        match self {
            Self::Text { anchor, head } => Self::Text {
                anchor: (*anchor).min(size),
                head: (*head).min(size),
            },
            Self::Node { anchor } => Self::Node {
                anchor: (*anchor).min(size.saturating_sub(1)),
            },
            Self::Cells {
                anchor_cell,
                head_cell,
            } => Self::Cells {
                anchor_cell: (*anchor_cell).min(size.saturating_sub(1)),
                head_cell: (*head_cell).min(size.saturating_sub(1)),
            },
            Self::All => Self::All,
        }
    }

    /// Validate this selection against a document.
    pub fn validate(&self, doc: &Node, schema: &Schema) -> RichTextResult<()> {
        let size = doc.content_size();
        match self {
            Self::Text { anchor, head } if *anchor <= size && *head <= size => Ok(()),
            Self::Node { anchor } if *anchor < size => {
                let Some(node) = node_after(doc, *anchor) else {
                    return Err(RichTextError::Selection(format!(
                        "node selection at {anchor} does not point to a node boundary"
                    )));
                };
                let node_type = schema.node_type(node.type_name())?;
                if node.is_text() || !node_type.is_selectable() {
                    return Err(RichTextError::Selection(format!(
                        "node selection at {anchor} targets node type `{}` which is not selectable",
                        node.type_name()
                    )));
                }
                Ok(())
            }
            Self::Cells {
                anchor_cell,
                head_cell,
            } => cell_selection::resolve(doc, schema, *anchor_cell, *head_cell).map(|_| ()),
            Self::All => Ok(()),
            Self::Text { anchor, head } => Err(RichTextError::Selection(format!(
                "text selection {anchor}..{head} exceeds document size {size}"
            ))),
            Self::Node { anchor } => Err(RichTextError::Selection(format!(
                "node selection at {anchor} exceeds document size {size}"
            ))),
        }
    }

    /// Normalized model ranges covered by this selection.
    ///
    /// A cell selection returns one content range per selected cell in
    /// row-major order. It never pretends a selected column is one linear
    /// range that also contains the unselected cells between its endpoints.
    pub fn ranges(&self, doc: &Node, schema: &Schema) -> RichTextResult<Vec<SelectionRange>> {
        match self {
            Self::Cells {
                anchor_cell,
                head_cell,
            } => Ok(cell_selection::resolve(doc, schema, *anchor_cell, *head_cell)?.ranges()),
            _ => Ok(vec![SelectionRange::new(self.from(doc), self.to(doc))]),
        }
    }

    /// Derived rectangle for a cell selection, or `None` for other kinds.
    pub fn cell_rect(
        &self,
        doc: &Node,
        schema: &Schema,
    ) -> RichTextResult<Option<CellSelectionRect>> {
        match self {
            Self::Cells {
                anchor_cell,
                head_cell,
            } => Ok(Some(
                cell_selection::resolve(doc, schema, *anchor_cell, *head_cell)?.rect(),
            )),
            _ => Ok(None),
        }
    }

    /// Absolute positions immediately before every selected cell, row-major.
    pub fn cell_positions(&self, doc: &Node, schema: &Schema) -> RichTextResult<Vec<usize>> {
        match self {
            Self::Cells {
                anchor_cell,
                head_cell,
            } => Ok(cell_selection::resolve(doc, schema, *anchor_cell, *head_cell)?.positions()),
            _ => Ok(Vec::new()),
        }
    }

    /// Copyable model content selected by this selection.
    ///
    /// Cell selections produce a closed table slice containing only the
    /// selected rectangle, with the original table, row, and cell attributes
    /// preserved.
    pub fn content(&self, doc: &Node, schema: &Schema) -> RichTextResult<Slice> {
        match self {
            Self::Cells {
                anchor_cell,
                head_cell,
            } => cell_selection::resolve(doc, schema, *anchor_cell, *head_cell)?.slice(),
            _ => doc.slice(self.from(doc), self.to(doc)),
        }
    }

    /// Convert to a bookmark.
    pub fn bookmark(&self) -> SelectionBookmark {
        match self {
            Self::Text { anchor, head } => SelectionBookmark::Text {
                anchor: *anchor,
                head: *head,
            },
            Self::Node { anchor } => SelectionBookmark::Node { anchor: *anchor },
            Self::Cells {
                anchor_cell,
                head_cell,
            } => SelectionBookmark::Cells {
                anchor_cell: *anchor_cell,
                head_cell: *head_cell,
            },
            Self::All => SelectionBookmark::All,
        }
    }
}

/// A normalized selection range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionRange {
    /// Range start.
    pub from: usize,
    /// Range end.
    pub to: usize,
}

impl SelectionRange {
    /// Create a sorted range.
    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            from: anchor.min(head),
            to: anchor.max(head),
        }
    }
}

/// A selection bookmark that can be mapped and resolved later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SelectionBookmark {
    /// Text bookmark.
    Text { anchor: usize, head: usize },
    /// Node bookmark.
    Node { anchor: usize },
    /// Semantic table-cell bookmark.
    Cells {
        anchor_cell: usize,
        head_cell: usize,
    },
    /// All-selection bookmark.
    All,
}

impl SelectionBookmark {
    /// Map a bookmark through a mapping.
    pub fn map(&self, mapping: &Mapping) -> Self {
        self.map_through_maps(&mapping.maps)
    }

    fn map_through_maps(&self, maps: &[StepMap]) -> Self {
        match self {
            Self::Text { anchor, head } => Self::Text {
                anchor: Mapping::map_through_maps(maps, *anchor, 1),
                head: Mapping::map_through_maps(maps, *head, 1),
            },
            Self::Node { anchor } => Self::Node {
                anchor: Mapping::map_through_maps(maps, *anchor, 1),
            },
            Self::Cells {
                anchor_cell,
                head_cell,
            } => Self::Cells {
                anchor_cell: Mapping::map_through_maps(maps, *anchor_cell, 1),
                head_cell: Mapping::map_through_maps(maps, *head_cell, 1),
            },
            Self::All => Self::All,
        }
    }

    /// Resolve a bookmark to a selection.
    pub fn resolve(&self, doc: &Node, schema: &Schema) -> RichTextResult<Selection> {
        let selection = match self {
            Self::Text { anchor, head } => Selection::Text {
                anchor: *anchor,
                head: *head,
            },
            Self::Node { anchor } => Selection::Node { anchor: *anchor },
            Self::Cells {
                anchor_cell,
                head_cell,
            } => Selection::Cells {
                anchor_cell: *anchor_cell,
                head_cell: *head_cell,
            },
            Self::All => Selection::All,
        };
        selection.validate(doc, schema)?;
        Ok(selection)
    }
}

fn node_after(doc: &Node, pos: usize) -> Option<Node> {
    doc.resolve(pos).ok()?.node_after()
}

fn resolve_mapped_selection(
    mapped: Selection,
    doc: &Node,
    schema: &Schema,
) -> RichTextResult<Selection> {
    match mapped.validate(doc, schema) {
        Ok(()) => Ok(mapped),
        Err(_) if matches!(&mapped, Selection::Node { .. }) => {
            let anchor = match mapped {
                Selection::Node { anchor } => anchor,
                _ => unreachable!("guard proves this is a node selection"),
            };
            Selection::near(doc, schema, anchor.min(doc.content_size()), 1)
        }
        Err(_) if mapped.is_cells() => {
            let head = match mapped {
                Selection::Cells { head_cell, .. } => head_cell,
                _ => unreachable!("guard proves this is a cell selection"),
            };
            // If a transform removed a selected cell or its table, keeping a
            // stale rectangular selection is worse than collapsing. Resolve
            // near the mapped head so later commands always receive a valid
            // model selection.
            Selection::near(doc, schema, head.min(doc.content_size()), 1)
        }
        Err(error) => Err(error),
    }
}

fn normalize_dir(dir: i8) -> i8 {
    if dir < 0 { -1 } else { 1 }
}

fn is_text_position(doc: &Node, schema: &Schema, pos: usize) -> RichTextResult<bool> {
    Ok(node_has_inline_content(schema, doc.resolve(pos)?.parent()))
}

fn node_has_inline_content(schema: &Schema, node: &Node) -> bool {
    schema
        .node_type(node.type_name())
        .is_ok_and(|node_type| node_type.inline_content(schema))
}

fn node_is_atom(schema: &Schema, node: &Node) -> bool {
    node.is_leaf()
        || schema
            .node_type(node.type_name())
            .is_ok_and(|node_type| node_type.is_atom())
}

fn node_is_selectable(schema: &Schema, node: &Node) -> bool {
    !node.is_text()
        && schema
            .node_type(node.type_name())
            .is_ok_and(|node_type| node_type.is_selectable())
}

fn find_selection_in(
    schema: &Schema,
    node: &Node,
    pos: usize,
    index: usize,
    dir: i8,
    text_only: bool,
) -> Option<Selection> {
    if node_has_inline_content(schema, node) {
        return Some(Selection::text(pos));
    }

    if dir > 0 {
        let mut pos = pos;
        for child_index in index..node.child_count() {
            let child = node.child(child_index)?;
            if !node_is_atom(schema, child) {
                if let Some(selection) =
                    find_selection_in(schema, child, pos + 1, 0, dir, text_only)
                {
                    return Some(selection);
                }
            } else if !text_only && node_is_selectable(schema, child) {
                return Some(Selection::node(pos));
            }
            pos += child.node_size();
        }
    } else {
        let mut pos = pos;
        for child_index in (0..index).rev() {
            let child = node.child(child_index)?;
            if !node_is_atom(schema, child) {
                if let Some(selection) = find_selection_in(
                    schema,
                    child,
                    pos.saturating_sub(1),
                    child.child_count(),
                    dir,
                    text_only,
                ) {
                    return Some(selection);
                }
            } else if !text_only && node_is_selectable(schema, child) {
                return Some(Selection::node(pos.saturating_sub(child.node_size())));
            }
            pos = pos.saturating_sub(child.node_size());
        }
    }

    None
}

/// A document transaction.
#[derive(Clone, Debug)]
pub struct Transaction {
    base_doc: Node,
    transform: Transform,
    selection: Option<Selection>,
    stored_marks: Option<Vec<Mark>>,
    meta: BTreeMap<String, Value>,
}

impl Transaction {
    /// Start a transaction.
    pub fn new(schema: Schema, doc: Node) -> Self {
        Self {
            base_doc: doc.clone(),
            transform: Transform::new(schema, doc),
            selection: None,
            stored_marks: None,
            meta: BTreeMap::new(),
        }
    }

    /// Current transaction document.
    pub fn doc(&self) -> &Node {
        self.transform.doc()
    }

    /// Underlying transform.
    pub fn transform(&self) -> &Transform {
        &self.transform
    }

    /// Document this transaction was created from.
    pub fn base_doc(&self) -> &Node {
        &self.base_doc
    }

    /// Transaction mapping.
    pub fn mapping(&self) -> Mapping {
        self.transform.mapping()
    }

    /// Explicit selection set by the transaction.
    pub fn selection(&self) -> Option<&Selection> {
        self.selection.as_ref()
    }

    /// Explicit stored marks set by the transaction.
    pub fn stored_marks(&self) -> Option<&[Mark]> {
        self.stored_marks.as_deref()
    }

    /// Transaction metadata.
    pub fn meta(&self, key: &str) -> Option<&Value> {
        self.meta.get(key)
    }

    /// All metadata.
    pub fn meta_map(&self) -> &BTreeMap<String, Value> {
        &self.meta
    }

    /// Set transaction metadata.
    pub fn set_meta(&mut self, key: impl Into<String>, value: Value) -> &mut Self {
        self.meta.insert(key.into(), value);
        self
    }

    /// Set selection.
    pub fn set_selection(&mut self, selection: Selection) -> RichTextResult<&mut Self> {
        selection.validate(self.doc(), self.transform.schema())?;
        if let Selection::Node { anchor } = &selection
            && let Some(node) = node_after(self.doc(), *anchor)
            && !self
                .transform
                .schema()
                .node_type(node.type_name())?
                .is_selectable()
        {
            return Err(RichTextError::Selection(format!(
                "node type {} is not selectable",
                node.type_name()
            )));
        }
        self.selection = Some(selection);
        self.stored_marks = None;
        Ok(self)
    }

    /// Set stored marks.
    pub fn set_stored_marks(&mut self, marks: Vec<Mark>) -> &mut Self {
        self.stored_marks = Some(marks);
        self
    }

    /// Replace content.
    pub fn replace(&mut self, from: usize, to: usize, slice: Slice) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.replace(from, to, slice)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Delete content.
    pub fn delete(&mut self, from: usize, to: usize) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.delete(from, to)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Insert content.
    pub fn insert(&mut self, pos: usize, content: Fragment) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.insert(pos, content)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Delete the current selection.
    pub fn delete_selection(&mut self) -> RichTextResult<&mut Self> {
        let selection = self.selection.clone().unwrap_or_else(|| Selection::text(0));
        if let Selection::Cells {
            anchor_cell,
            head_cell,
        } = &selection
        {
            let ranges = cell_selection::resolve(
                self.doc(),
                self.transform.schema(),
                *anchor_cell,
                *head_cell,
            )?
            .ranges();
            // Work backwards so each not-yet-cleared cell keeps its original
            // coordinates. Every replace maps the explicit cell selection,
            // preserving it as semantic cell positions.
            for range in ranges.into_iter().rev() {
                if range.from == range.to {
                    continue;
                }
                let cell =
                    node_after(self.doc(), range.from.saturating_sub(1)).ok_or_else(|| {
                        RichTextError::Selection(
                            "selected cell disappeared while clearing its content".to_string(),
                        )
                    })?;
                let empty = self
                    .transform
                    .schema()
                    .node_type(cell.type_name())?
                    .content_expr()
                    .fill_before(self.transform.schema(), &Fragment::empty(), true)
                    .ok_or_else(|| {
                        RichTextError::Selection(format!(
                            "cell type `{}` has no valid empty/default content",
                            cell.type_name()
                        ))
                    })?;
                self.replace_with(range.from, range.to, empty)?;
            }
            return Ok(self);
        }
        let from = selection.from(self.doc());
        let to = selection.to(self.doc());
        let collapse_deleted_node = matches!(selection, Selection::Node { .. });
        let preserved_marks = if matches!(selection, Selection::Text { .. }) && from < to {
            let from_pos = self.doc().resolve(from)?;
            let to_pos = self.doc().resolve(to)?;
            from_pos.marks_across(&to_pos, self.transform.schema())
        } else {
            None
        };

        self.delete(from, to)?;
        if collapse_deleted_node {
            // Mapping a Node selection through deletion can leave it anchored
            // on the text/atom that shifted into the removed node's position.
            // Deleting the selected node instead collapses to the nearest real
            // caret at its former boundary.
            self.set_selection(Selection::near(
                self.doc(),
                self.transform.schema(),
                from.min(self.doc().content_size()),
                1,
            )?)?;
        }
        if let Some(marks) = preserved_marks {
            self.set_stored_marks(marks);
        }
        Ok(self)
    }

    /// Replace the current selection with a slice.
    pub fn replace_selection(&mut self, slice: Slice) -> RichTextResult<&mut Self> {
        let selection = self.selection.clone().unwrap_or_else(|| Selection::text(0));
        if selection.is_cells() {
            return self.replace_cell_selection(slice);
        }
        let from = selection.from(self.doc());
        let to = selection.to(self.doc());
        let bias = slice_insertion_bias(&slice, self.transform.schema());
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.replace_range(from, to, slice)?;
        self.map_selection_from(map_start)?;
        self.selection_to_insertion_end(map_start, bias)?;
        Ok(self)
    }

    /// Replace the current selection with a node.
    pub fn replace_selection_with(&mut self, node: Node) -> RichTextResult<&mut Self> {
        let selection = self.selection.clone().unwrap_or_else(|| Selection::text(0));
        let mut node = node;
        if self
            .transform
            .schema()
            .node_type(node.type_name())
            .is_ok_and(|node_type| node_type.is_inline())
        {
            node = node.with_marks(self.marks_for_selection(&selection)?);
        }
        self.replace_selection(Slice::new(Fragment::from(node), 0, 0))
    }

    /// Replace the current selection with plain text.
    pub fn insert_text(&mut self, text: impl Into<String>) -> RichTextResult<&mut Self> {
        let selection = self.selection.clone().unwrap_or_else(|| Selection::text(0));
        let marks = self.marks_for_selection(&selection)?;
        let node = self.transform.schema().text(text, marks)?;
        self.replace_selection_with(node)
    }

    /// Push a single pre-built step onto the transaction. Used by
    /// `history::undo` / `redo` to replay already-inverted steps without
    /// rebuilding them from a higher-level operation.
    pub fn step(&mut self, step: crate::transform::Step) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.step(step)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Lift the closest block ancestor of `[from..to]` out of its parent.
    /// Mirrors PM's `Transaction.lift(range, target)` collapsed to pine's
    /// position-style API.
    pub fn lift(&mut self, from: usize, to: usize) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.lift(from, to)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Join the boundary at `pos` with the block before it.
    pub fn join(&mut self, pos: usize) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.join(pos)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Split the node at `pos` at the given depth.
    pub fn split(&mut self, pos: usize, depth: usize) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.split(pos, depth)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Split the node at `pos` while overriding right-side node types.
    pub fn split_into(
        &mut self,
        pos: usize,
        depth: usize,
        types_after: &[Option<crate::transform::TypeAfter>],
    ) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.split_into(pos, depth, types_after)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Wrap `[from..to]` in a node of the given type.
    pub fn wrap(
        &mut self,
        from: usize,
        to: usize,
        node_type: &str,
        attrs: Attrs,
    ) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.wrap(from, to, node_type, attrs)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Wrap `[from..to]` in a chain of wrappers (outer-first). See
    /// [`crate::transform::Transform::wrap_chain`].
    pub fn wrap_chain<I>(&mut self, from: usize, to: usize, chain: I) -> RichTextResult<&mut Self>
    where
        I: IntoIterator<Item = crate::transform::WrapperSpec>,
    {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.wrap_chain(from, to, chain)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Replace `[from..to]` with the given content fragment.
    pub fn replace_with(
        &mut self,
        from: usize,
        to: usize,
        content: Fragment,
    ) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.replace_with(from, to, content)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Change the type of every textblock in `[from..to]`.
    pub fn set_block_type(
        &mut self,
        from: usize,
        to: usize,
        node_type: &str,
        attrs: Attrs,
    ) -> RichTextResult<&mut Self> {
        let map_start = self.transform.maps().len();
        self.stored_marks = None;
        self.transform.set_block_type(from, to, node_type, attrs)?;
        self.map_selection_from(map_start)?;
        Ok(self)
    }

    /// Add a mark.
    pub fn add_mark(&mut self, from: usize, to: usize, mark: Mark) -> RichTextResult<&mut Self> {
        self.stored_marks = None;
        self.transform.add_mark(from, to, mark)?;
        Ok(self)
    }

    /// Remove a mark.
    pub fn remove_mark(&mut self, from: usize, to: usize, mark: Mark) -> RichTextResult<&mut Self> {
        self.stored_marks = None;
        self.transform.remove_mark(from, to, mark)?;
        Ok(self)
    }

    fn map_selection_from(&mut self, map_start: usize) -> RichTextResult<()> {
        if let Some(selection) = &self.selection {
            let mapped = selection
                .map_through_maps(&self.transform.maps()[map_start..])
                .clamped(self.doc());
            self.selection = Some(resolve_mapped_selection(
                mapped,
                self.doc(),
                self.transform.schema(),
            )?);
        }
        Ok(())
    }

    fn selection_to_insertion_end(&mut self, map_start: usize, bias: i8) -> RichTextResult<()> {
        let Some(last_index) = self.transform.maps().len().checked_sub(1) else {
            return Ok(());
        };
        if last_index < map_start {
            return Ok(());
        }

        let mut end = None;
        self.transform.maps()[last_index].for_each(|_, _, _, new_to| {
            if end.is_none() {
                end = Some(new_to);
            }
        });
        if let Some(end) = end {
            self.selection = Some(Selection::near(
                self.doc(),
                self.transform.schema(),
                end,
                bias,
            )?);
        }
        Ok(())
    }

    fn marks_for_selection(&self, selection: &Selection) -> RichTextResult<Vec<Mark>> {
        if selection.is_cells() {
            return Ok(Vec::new());
        }
        if let Some(marks) = &self.stored_marks {
            return Ok(marks.clone());
        }

        let from = selection.from(self.doc());
        let to = selection.to(self.doc());
        let from_pos = self.doc().resolve(from)?;
        if from == to {
            Ok(from_pos.marks(self.transform.schema()))
        } else {
            Ok(from_pos
                .marks_across(&self.doc().resolve(to)?, self.transform.schema())
                .unwrap_or_default())
        }
    }

    fn replace_cell_selection(&mut self, slice: Slice) -> RichTextResult<&mut Self> {
        let (anchor_cell, head_cell) = match self.selection.as_ref() {
            Some(Selection::Cells {
                anchor_cell,
                head_cell,
            }) => (*anchor_cell, *head_cell),
            _ => {
                return Err(RichTextError::Selection(
                    "rectangular replacement requires a cell selection".to_string(),
                ));
            }
        };
        let selected =
            cell_selection::resolve(self.doc(), self.transform.schema(), anchor_cell, head_cell)?;
        if let Some(replacements) =
            selected.rectangular_replacements(&slice, self.transform.schema())?
        {
            for (range, content) in replacements.into_iter().rev() {
                self.replace_with(range.from, range.to, content)?;
            }
            return Ok(self);
        }

        self.delete_selection()?;
        let insertion = match self.selection.as_ref() {
            Some(Selection::Cells { anchor_cell, .. }) => *anchor_cell + 1,
            _ => {
                return Err(RichTextError::Selection(
                    "cell selection was lost while preparing replacement".to_string(),
                ));
            }
        };
        self.set_selection(Selection::text(insertion))?;
        if slice.size() == 0 {
            return Ok(self);
        }

        let bias = slice_insertion_bias(&slice, self.transform.schema());
        let map_start = self.transform.maps().len();
        self.transform.replace_range(insertion, insertion, slice)?;
        self.map_selection_from(map_start)?;
        self.selection_to_insertion_end(map_start, bias)?;
        Ok(self)
    }
}

fn slice_insertion_bias(slice: &Slice, schema: &Schema) -> i8 {
    let mut last = slice.content.as_slice().last();
    let mut last_parent = None;
    for _ in 0..slice.open_end {
        last_parent = last;
        last = last.and_then(|node| node.content().as_slice().last());
    }

    let inline_tail = last.is_some_and(|node| {
        schema
            .node_type(node.type_name())
            .is_ok_and(|node_type| node_type.is_inline())
    }) || last_parent.is_some_and(|node| {
        schema
            .node_type(node.type_name())
            .is_ok_and(|node_type| node_type.inline_content(schema))
    });
    if inline_tail { -1 } else { 1 }
}

/// Configuration for [`EditorState::create`].
#[derive(Clone)]
pub struct EditorStateConfig {
    /// Schema.
    pub schema: Schema,
    /// Initial document.
    pub doc: Node,
    /// Initial selection. Defaults to a text cursor at position 0.
    pub selection: Option<Selection>,
    /// Initial stored marks.
    pub stored_marks: Option<Vec<Mark>>,
    /// Pure plugins.
    pub plugins: Vec<Plugin>,
}

impl EditorStateConfig {
    /// Create a state config.
    pub fn new(schema: Schema, doc: Node) -> Self {
        Self {
            schema,
            doc,
            selection: None,
            stored_marks: None,
            plugins: Vec::new(),
        }
    }

    /// Set selection.
    pub fn selection(mut self, selection: Selection) -> Self {
        self.selection = Some(selection);
        self
    }

    /// Set plugins.
    pub fn plugins(mut self, plugins: Vec<Plugin>) -> Self {
        self.plugins = plugins;
        self
    }
}

/// Editor state with document, selection, marks, and pure plugin state.
#[derive(Clone)]
pub struct EditorState {
    schema: Schema,
    doc: Node,
    selection: Selection,
    stored_marks: Option<Vec<Mark>>,
    plugins: Vec<Plugin>,
    /// Plugin state Values are wrapped in `Arc` so `EditorState::clone`
    /// is O(plugins) instead of O(sum-of-plugin-state-sizes). The
    /// `history` plugin in particular grows its Value monotonically
    /// as the user types, and the dispatch / state_provider closures
    /// both clone the state on every keystroke — without `Arc` here,
    /// each clone walked the whole history tree.
    plugin_state: BTreeMap<String, Arc<Value>>,
}

impl fmt::Debug for EditorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EditorState")
            .field("doc", &self.doc)
            .field("selection", &self.selection)
            .field("stored_marks", &self.stored_marks)
            .field("plugin_state", &self.plugin_state)
            .finish_non_exhaustive()
    }
}

impl EditorState {
    /// Create editor state.
    pub fn create(config: EditorStateConfig) -> RichTextResult<Self> {
        config.schema.check_node(&config.doc)?;
        let selection = config
            .selection
            .unwrap_or_else(|| Selection::at_start(&config.doc, &config.schema));
        selection.validate(&config.doc, &config.schema)?;

        let mut state = Self {
            schema: config.schema,
            doc: config.doc,
            selection,
            stored_marks: config.stored_marks,
            plugins: config.plugins,
            plugin_state: BTreeMap::new(),
        };

        for plugin in &state.plugins {
            if let Some(field) = &plugin.state_field {
                let value = field.init(&state)?;
                state
                    .plugin_state
                    .insert(plugin.key.clone(), Arc::new(value));
            }
        }

        Ok(state)
    }

    /// Create a transaction from this state.
    pub fn tr(&self) -> Transaction {
        let mut transaction = Transaction::new(self.schema.clone(), self.doc.clone());
        transaction.selection = Some(self.selection.clone());
        transaction.stored_marks = self.stored_marks.clone();
        transaction
    }

    /// Schema.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Current document.
    pub fn doc(&self) -> &Node {
        &self.doc
    }

    /// Current selection.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Current stored marks.
    pub fn stored_marks(&self) -> Option<&[Mark]> {
        self.stored_marks.as_deref()
    }

    /// Plugin state value by key.
    pub fn plugin_state(&self, key: &str) -> Option<&Value> {
        self.plugin_state.get(key).map(|v| &**v)
    }

    /// Return a new state with `selection` substituted for the current
    /// one. Skips the plugin state field loop and transaction
    /// filter/append hooks — appropriate for callers that want to
    /// inject the live DOM cursor before running a command, where no
    /// transform step is involved. Validates the selection against
    /// the doc.
    ///
    /// This is cheaper than `state.tr() + tr.set_selection + state.apply`
    /// for two reasons: it avoids the upfront `BTreeMap::clone` of
    /// plugin_state inside `apply_without_append`, and it skips the
    /// plugin loop entirely (every plugin's `apply_in_place` is a
    /// no-op for an empty-step transaction, but the loop still runs
    /// and the prior remove-then-reinsert dance still touches every
    /// entry).
    pub fn with_selection(&self, selection: Selection) -> RichTextResult<Self> {
        selection.validate(&self.doc, &self.schema)?;
        let stores_marks = matches!(&selection, Selection::Text { anchor, head } if anchor == head);
        let mut next = self.clone();
        next.selection = selection;
        if !stores_marks {
            next.stored_marks = None;
        }
        Ok(next)
    }

    /// Apply a transaction, including append-transaction hooks.
    pub fn apply_transaction(
        &self,
        transaction: Transaction,
    ) -> RichTextResult<(Self, Vec<Transaction>)> {
        self.apply_inner(transaction, true)
    }

    /// Apply a transaction and return only the new state.
    pub fn apply(&self, transaction: Transaction) -> RichTextResult<Self> {
        self.apply_transaction(transaction).map(|(state, _)| state)
    }

    /// Reconfigure plugins while preserving matching plugin state.
    pub fn reconfigure(&self, plugins: Vec<Plugin>) -> RichTextResult<Self> {
        let mut state = self.clone();
        state.plugins = plugins;
        let mut next_plugin_state = BTreeMap::new();
        for plugin in &state.plugins {
            if let Some(existing) = self.plugin_state.get(&plugin.key) {
                next_plugin_state.insert(plugin.key.clone(), Arc::clone(existing));
            } else if let Some(field) = &plugin.state_field {
                next_plugin_state.insert(plugin.key.clone(), Arc::new(field.init(&state)?));
            }
        }
        state.plugin_state = next_plugin_state;
        Ok(state)
    }

    /// Serialize state in the Rust-native format.
    pub fn to_json(&self) -> RichTextResult<Value> {
        // `Arc<Value>` doesn't implement `Serialize` without serde's
        // `rc` feature; build a borrowed view that serde can walk
        // directly. Cheap — references only.
        let plugin_state: BTreeMap<&String, &Value> =
            self.plugin_state.iter().map(|(k, v)| (k, &**v)).collect();
        Ok(json!({
            "doc": self.doc,
            "selection": self.selection,
            "stored_marks": self.stored_marks,
            "plugin_state": plugin_state,
        }))
    }

    /// Deserialize state in the Rust-native format.
    pub fn from_json(schema: Schema, plugins: Vec<Plugin>, value: Value) -> RichTextResult<Self> {
        #[derive(Deserialize)]
        struct StateJson {
            doc: WireNode,
            selection: Option<Selection>,
            stored_marks: Option<Vec<Mark>>,
            #[serde(default)]
            plugin_state: BTreeMap<String, Value>,
        }

        let decoded: StateJson = serde_json::from_value(value)?;
        let doc = schema.materialize_wire_node(decoded.doc)?;
        let mut state = Self::create(EditorStateConfig {
            schema,
            doc,
            selection: decoded.selection,
            stored_marks: decoded.stored_marks,
            plugins,
        })?;
        for (key, value) in decoded.plugin_state {
            if state.plugins.iter().any(|plugin| plugin.key == key) {
                state.plugin_state.insert(key, Arc::new(value));
            }
        }
        Ok(state)
    }

    fn apply_inner(
        &self,
        transaction: Transaction,
        allow_append: bool,
    ) -> RichTextResult<(Self, Vec<Transaction>)> {
        for plugin in &self.plugins {
            if let Some(filter) = &plugin.filter_transaction
                && !(filter)(&transaction, self)?
            {
                return Ok((self.clone(), Vec::new()));
            }
        }

        let mut next = self.apply_without_append(&transaction)?;
        let mut transactions = vec![transaction];

        if allow_append {
            for _ in 0..16 {
                let mut appended = None;
                for plugin in &next.plugins {
                    if let Some(append) = &plugin.append_transaction
                        && let Some(transaction) = (append)(&transactions, self, &next)?
                    {
                        appended = Some(transaction);
                        break;
                    }
                }

                let Some(mut transaction) = appended else {
                    break;
                };
                if transaction.meta(META_APPENDED_TRANSACTION).is_none() {
                    transaction.set_meta(META_APPENDED_TRANSACTION, Value::from(0_u64));
                }
                let (state, mut applied) = next.apply_inner(transaction, false)?;
                if applied.is_empty() {
                    break;
                }
                next = state;
                transactions.append(&mut applied);
            }
        }

        Ok((next, transactions))
    }

    fn apply_without_append(&self, transaction: &Transaction) -> RichTextResult<Self> {
        if transaction.base_doc() != self.doc() {
            return Err(RichTextError::Transform(
                "transaction was created from a different document".to_string(),
            ));
        }

        let selection = if let Some(selection) = transaction.selection.clone() {
            selection
        } else {
            let mapped = self
                .selection
                .map_through_maps(transaction.transform().maps())
                .clamped(transaction.doc());
            resolve_mapped_selection(mapped, transaction.doc(), &self.schema)?
        };
        selection.validate(transaction.doc(), &self.schema)?;

        let stores_marks = matches!(&selection, Selection::Text { anchor, head } if anchor == head);
        let mut next = Self {
            schema: self.schema.clone(),
            doc: transaction.doc().clone(),
            selection,
            stored_marks: if stores_marks {
                transaction.stored_marks.clone()
            } else {
                None
            },
            plugins: self.plugins.clone(),
            plugin_state: self.plugin_state.clone(),
        };

        // Plugin state fields run via `apply_in_place` so plugins that
        // override it (e.g. `history_plugin`) can mutate their Value in
        // place — pushing one event to a Vec — instead of paying
        // serde_json::from_value + to_value over the entire (growing)
        // plugin state every commit. Plugins that don't override
        // `apply_in_place` keep the original semantics through the
        // default impl.
        //
        // The plugin_state values are `Arc<Value>`, so `make_mut`
        // clones the underlying Value only when it's shared with
        // another state — the common case during `state.apply` (the
        // new state shares Arc with the old). Plugins that early-
        // return inside `apply_in_place` without writing avoid the
        // clone entirely.
        let plugin_keys: Vec<(String, Arc<dyn StateField>)> = self
            .plugins
            .iter()
            .filter_map(|p| p.state_field.as_ref().map(|f| (p.key.clone(), f.clone())))
            .collect();
        for (key, field) in plugin_keys {
            let mut value = next
                .plugin_state
                .remove(&key)
                .unwrap_or_else(|| Arc::new(Value::Null));
            // `make_mut` clones the underlying Value if multiple Arcs
            // share it; otherwise returns the existing storage in
            // place. The clone cost is paid per-mutating-plugin, not
            // per-state-apply, which matters when a plugin's value
            // grows monotonically (history).
            let value_ref = Arc::make_mut(&mut value);
            field.apply_in_place(transaction, value_ref, self, &next)?;
            next.plugin_state.insert(key, value);
        }

        Ok(next)
    }
}

/// Pure plugin state field.
pub trait StateField: Send + Sync {
    /// Initialize plugin state.
    fn init(&self, state: &EditorState) -> RichTextResult<Value>;

    /// Apply a transaction to plugin state.
    fn apply(
        &self,
        transaction: &Transaction,
        value: &Value,
        old_state: &EditorState,
        new_state: &EditorState,
    ) -> RichTextResult<Value>;

    /// Apply a transaction to plugin state, mutating the current
    /// value in place. Override this to skip the
    /// `clone → from_value → mutate → to_value` round-trip when a
    /// plugin can update its representation cheaply
    /// — `history_plugin` is the canonical example: pushing a single
    /// event to `done.as_array_mut()` is O(1), whereas the
    /// fallback `apply` deserializes and re-serializes the full
    /// (and growing) history on every commit.
    ///
    /// The default implementation routes through `apply` for
    /// backwards compatibility with plugins written before this
    /// method existed.
    fn apply_in_place(
        &self,
        transaction: &Transaction,
        value: &mut Value,
        old_state: &EditorState,
        new_state: &EditorState,
    ) -> RichTextResult<()> {
        let new = self.apply(transaction, value, old_state, new_state)?;
        *value = new;
        Ok(())
    }
}

impl<F, G> StateField for StateFieldFns<F, G>
where
    F: Fn(&EditorState) -> RichTextResult<Value> + Send + Sync,
    G: Fn(&Transaction, &Value, &EditorState, &EditorState) -> RichTextResult<Value> + Send + Sync,
{
    fn init(&self, state: &EditorState) -> RichTextResult<Value> {
        (self.init)(state)
    }

    fn apply(
        &self,
        transaction: &Transaction,
        value: &Value,
        old_state: &EditorState,
        new_state: &EditorState,
    ) -> RichTextResult<Value> {
        (self.apply)(transaction, value, old_state, new_state)
    }
}

/// Closure-backed state field.
pub struct StateFieldFns<F, G> {
    init: F,
    apply: G,
}

/// Closure-backed state field whose apply hook mutates the existing
/// value in place. The default `apply` method clones first, so plugins
/// that build this from already-allocated Value subtrees still get the
/// allocation savings — only the original `Value::clone` survives, not
/// a full `serde_json::from_value` + `to_value` round-trip over the
/// whole field state.
pub struct InPlaceStateFieldFns<I, A> {
    init: I,
    apply_in_place: A,
}

impl<I, A> StateField for InPlaceStateFieldFns<I, A>
where
    I: Fn(&EditorState) -> RichTextResult<Value> + Send + Sync,
    A: Fn(&Transaction, &mut Value, &EditorState, &EditorState) -> RichTextResult<()> + Send + Sync,
{
    fn init(&self, state: &EditorState) -> RichTextResult<Value> {
        (self.init)(state)
    }

    fn apply(
        &self,
        transaction: &Transaction,
        value: &Value,
        old_state: &EditorState,
        new_state: &EditorState,
    ) -> RichTextResult<Value> {
        let mut owned = value.clone();
        (self.apply_in_place)(transaction, &mut owned, old_state, new_state)?;
        Ok(owned)
    }

    fn apply_in_place(
        &self,
        transaction: &Transaction,
        value: &mut Value,
        old_state: &EditorState,
        new_state: &EditorState,
    ) -> RichTextResult<()> {
        (self.apply_in_place)(transaction, value, old_state, new_state)
    }
}

type FilterHook =
    Arc<dyn Fn(&Transaction, &EditorState) -> RichTextResult<bool> + Send + Sync + 'static>;
type AppendHook = Arc<
    dyn Fn(&[Transaction], &EditorState, &EditorState) -> RichTextResult<Option<Transaction>>
        + Send
        + Sync
        + 'static,
>;

/// A pure editor plugin.
#[derive(Clone)]
pub struct Plugin {
    key: String,
    state_field: Option<Arc<dyn StateField>>,
    filter_transaction: Option<FilterHook>,
    append_transaction: Option<AppendHook>,
}

impl fmt::Debug for Plugin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plugin")
            .field("key", &self.key)
            .field("has_state_field", &self.state_field.is_some())
            .field("has_filter_transaction", &self.filter_transaction.is_some())
            .field("has_append_transaction", &self.append_transaction.is_some())
            .finish()
    }
}

impl Plugin {
    /// Start a plugin builder.
    pub fn builder(key: impl Into<String>) -> PluginBuilder {
        PluginBuilder::new(key)
    }

    /// Plugin key.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Builder for [`Plugin`].
pub struct PluginBuilder {
    key: String,
    state_field: Option<Arc<dyn StateField>>,
    filter_transaction: Option<FilterHook>,
    append_transaction: Option<AppendHook>,
}

impl PluginBuilder {
    /// Create a plugin builder.
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            state_field: None,
            filter_transaction: None,
            append_transaction: None,
        }
    }

    /// Add a state field from closures.
    pub fn state_field<F, G>(mut self, init: F, apply: G) -> Self
    where
        F: Fn(&EditorState) -> RichTextResult<Value> + Send + Sync + 'static,
        G: Fn(&Transaction, &Value, &EditorState, &EditorState) -> RichTextResult<Value>
            + Send
            + Sync
            + 'static,
    {
        self.state_field = Some(Arc::new(StateFieldFns { init, apply }));
        self
    }

    /// Add an in-place state field. The apply closure takes a mutable
    /// reference to the existing Value and updates it directly, which
    /// is the cheap path for plugins whose state is a growing
    /// collection (history is the canonical example: pushing one event
    /// to `done` is O(1), full round-trip is O(N)).
    pub fn state_field_in_place<I, A>(mut self, init: I, apply: A) -> Self
    where
        I: Fn(&EditorState) -> RichTextResult<Value> + Send + Sync + 'static,
        A: Fn(&Transaction, &mut Value, &EditorState, &EditorState) -> RichTextResult<()>
            + Send
            + Sync
            + 'static,
    {
        self.state_field = Some(Arc::new(InPlaceStateFieldFns {
            init,
            apply_in_place: apply,
        }));
        self
    }

    /// Add a transaction filter hook.
    pub fn filter_transaction<F>(mut self, filter: F) -> Self
    where
        F: Fn(&Transaction, &EditorState) -> RichTextResult<bool> + Send + Sync + 'static,
    {
        self.filter_transaction = Some(Arc::new(filter));
        self
    }

    /// Add an append-transaction hook.
    pub fn append_transaction<F>(mut self, append: F) -> Self
    where
        F: Fn(&[Transaction], &EditorState, &EditorState) -> RichTextResult<Option<Transaction>>
            + Send
            + Sync
            + 'static,
    {
        self.append_transaction = Some(Arc::new(append));
        self
    }

    /// Finish the plugin.
    pub fn finish(self) -> Plugin {
        Plugin {
            key: self.key,
            state_field: self.state_field,
            filter_transaction: self.filter_transaction,
            append_transaction: self.append_transaction,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::{Fragment, MarkPolicy, NodeSpec, Slice};
    use crate::schema_basic;

    fn state() -> EditorState {
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![schema_basic::text("hello", Vec::new()).unwrap()])
                .unwrap(),
        ])
        .unwrap();
        EditorState::create(EditorStateConfig::new(schema, doc)).unwrap()
    }

    #[test]
    fn transaction_maps_selection() {
        let state = state();
        let mut tr = state.tr();
        tr.set_selection(Selection::text(6)).unwrap();
        let insert = Fragment::from(schema_basic::text("!", Vec::new()).unwrap());
        tr.replace(6, 6, Slice::new(insert, 0, 0)).unwrap();

        let next = state.apply(tr).unwrap();
        assert_eq!(next.selection(), &Selection::text(7));
        assert_eq!(next.doc().text_content(), "hello!");
    }

    #[test]
    fn editor_state_default_selection_starts_inside_first_textblock() {
        let state = state();

        assert_eq!(state.selection(), &Selection::text(1));
    }

    #[test]
    fn editor_state_default_selection_enters_required_title_node() {
        let schema = crate::model::Schema::builder()
            .node(NodeSpec::new("doc").content("title block*"))
            .node(
                NodeSpec::new("title")
                    .group("title")
                    .content("inline*")
                    .marks(MarkPolicy::None),
            )
            .node(NodeSpec::new("paragraph").group("block").content("inline*"))
            .node(NodeSpec::new("text").group("inline").inline())
            .finish()
            .unwrap();
        let title = schema
            .node("title", Attrs::new(), Fragment::empty())
            .unwrap();
        let doc = schema
            .node("doc", Attrs::new(), Fragment::from(title))
            .unwrap();
        let state = EditorState::create(EditorStateConfig::new(schema, doc)).unwrap();

        assert_eq!(state.selection(), &Selection::text(1));
    }

    #[test]
    fn transaction_insert_maps_explicit_selection() {
        let state = state();
        let mut tr = state.tr();
        tr.set_selection(Selection::text(6)).unwrap();
        let insert = Fragment::from(schema_basic::text("!", Vec::new()).unwrap());
        tr.insert(1, insert).unwrap();

        assert_eq!(tr.selection(), Some(&Selection::text(7)));
        let next = state.apply(tr).unwrap();
        assert_eq!(next.selection(), &Selection::text(7));
        assert_eq!(next.doc().text_content(), "!hello");
    }

    #[test]
    fn transaction_delete_maps_explicit_selection() {
        let state = state();
        let mut tr = state.tr();
        tr.set_selection(Selection::text(6)).unwrap();
        tr.delete(1, 2).unwrap();

        assert_eq!(tr.selection(), Some(&Selection::text(5)));
        let next = state.apply(tr).unwrap();
        assert_eq!(next.selection(), &Selection::text(5));
        assert_eq!(next.doc().text_content(), "ello");
    }

    #[test]
    fn transaction_deletes_and_replaces_current_selection() {
        let state = state();
        let mut tr = state.tr();
        tr.set_selection(Selection::text_between(2, 4)).unwrap();
        tr.delete_selection().unwrap();

        let next = state.apply(tr).unwrap();
        assert_eq!(next.doc().text_content(), "hlo");
        assert_eq!(next.selection(), &Selection::text(2));

        let mut tr = next.tr();
        tr.set_selection(Selection::text(2)).unwrap();
        tr.insert_text("ey").unwrap();
        let next = next.apply(tr).unwrap();
        assert_eq!(next.doc().text_content(), "heylo");
        assert_eq!(next.selection(), &Selection::text(4));
    }

    #[test]
    fn transaction_insert_text_preserves_consecutive_spaces() {
        let state = state();
        let mut tr = state.tr();
        tr.set_selection(Selection::text(6)).unwrap();
        tr.insert_text("  ").unwrap();

        let next = state.apply(tr).unwrap();
        assert_eq!(next.doc().text_content(), "hello  ");
        assert_eq!(next.selection(), &Selection::text(8));
    }

    #[test]
    fn node_selection_covers_the_selected_node_size() {
        let state = state();
        let selection = Selection::node(0);
        selection.validate(state.doc(), state.schema()).unwrap();

        assert_eq!(selection.from(state.doc()), 0);
        assert_eq!(selection.to(state.doc()), 7);
        assert!(!selection.is_empty(state.doc()));
    }

    #[test]
    fn rejects_transaction_from_different_document() {
        let state = state();
        let mut stale_transaction = state.tr();
        stale_transaction.delete(1, 2).unwrap();

        let mut first_transaction = state.tr();
        first_transaction.delete(2, 3).unwrap();
        let next = state.apply(first_transaction).unwrap();

        let err = next.apply(stale_transaction).unwrap_err();
        assert!(
            err.to_string()
                .contains("transaction was created from a different document")
        );
    }

    #[test]
    fn plugin_state_and_filter_hooks_run() {
        let plugin = Plugin::builder("count")
            .state_field(
                |_| Ok(json!(0)),
                |transaction, value, _, _| {
                    let count = value.as_u64().unwrap_or(0);
                    Ok(json!(count + transaction.transform().steps().len() as u64))
                },
            )
            .filter_transaction(|transaction, _| {
                Ok(transaction.meta("reject") != Some(&json!(true)))
            })
            .finish();

        let state = state().reconfigure(vec![plugin]).unwrap();
        let mut tr = state.tr();
        tr.delete(1, 2).unwrap();
        let next = state.apply(tr).unwrap();
        assert_eq!(next.plugin_state("count"), Some(&json!(1)));

        let mut rejected = next.tr();
        rejected.set_meta("reject", json!(true));
        let (same, applied) = next.apply_transaction(rejected).unwrap();
        assert!(applied.is_empty());
        assert_eq!(same.doc(), next.doc());
    }

    #[test]
    fn state_json_round_trip() {
        let state = state();
        let value = state.to_json().unwrap();
        let decoded = EditorState::from_json(schema_basic::schema(), Vec::new(), value).unwrap();
        assert_eq!(decoded.doc(), state.doc());
        assert_eq!(decoded.selection(), state.selection());
    }
}
