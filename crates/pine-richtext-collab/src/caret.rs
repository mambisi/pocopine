//! `StickyIndex` carets — positions that survive concurrent edits (Phase 5 / A2).
//!
//! A model caret is an absolute integer offset into the *current* document; it
//! shifts whenever anyone edits before it. A [`StickyPoint`] anchors that offset
//! to the CRDT itself — the block's stable `block_id` plus a `yrs::StickyIndex`
//! into that block's text — so after a remote edit merges in, resolving it yields
//! the *new* offset of the same logical spot. That is what lets a writer keep
//! their caret while a peer types elsewhere (and what a live remote cursor needs).
//!
//! This only works on top of the fine-diff write ([`step_writer`](crate::step_writer)):
//! the coarse rebuild tombstones every item and re-mints `block_id`s, which
//! invalidates a `StickyPoint` (its block can no longer be found). The caller then
//! re-derives the caret from the editor selection.

use pine_richtext::model::{Node, Schema};
use serde::{Deserialize, Serialize};
use yrs::types::xml::{Xml, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut};
use yrs::{Any, Assoc, IndexedSequence, Out, ReadTxn, StickyIndex};

use crate::BLOCK_ID_KEY;
use crate::step_writer::flat_block_offset;

/// A caret/selection endpoint anchored to a CRDT position so it tracks the
/// document through concurrent edits. Serde-encodable, so it also rides an
/// awareness frame as a remote peer's live cursor.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StickyPoint {
    /// The stable id of the block the point sits in.
    pub block_id: String,
    /// A `yrs::StickyIndex` into that block's text.
    pub sticky: StickyIndex,
}

/// Anchor an absolute model position as a [`StickyPoint`], or `None` if it isn't
/// strictly inside a flat textblock (block boundaries / nested blocks can't be
/// anchored — the caller falls back to a raw offset there).
pub(crate) fn point_at<T: ReadTxn>(
    txn: &T,
    root: &XmlFragmentRef,
    doc: &Node,
    schema: &Schema,
    model_pos: usize,
) -> Option<StickyPoint> {
    let (block_index, offset) = flat_block_offset(doc, schema, model_pos)?;
    let XmlOut::Element(element) = root.get(txn, block_index as u32)? else {
        return None;
    };
    let block_id = block_id_of(txn, &element)?;
    let XmlOut::Text(xtext) = element.get(txn, 0)? else {
        return None;
    };
    let sticky = xtext.sticky_index(txn, offset, Assoc::After)?;
    Some(StickyPoint { block_id, sticky })
}

/// Resolve a [`StickyPoint`] to an absolute model position in the current
/// document, or `None` if its block is gone (e.g. a coarse rebuild re-minted ids).
pub(crate) fn point_model_pos<T: ReadTxn>(
    txn: &T,
    root: &XmlFragmentRef,
    doc: &Node,
    point: &StickyPoint,
) -> Option<usize> {
    let block_index = block_index_by_id(txn, root, &point.block_id)?;
    let offset = point.sticky.get_offset(txn)?.index as usize;
    // The block's content starts one token (its open tag) past the sum of the
    // sizes of the blocks before it.
    let mut base = 0usize;
    for i in 0..block_index {
        base += doc.child(i)?.node_size();
    }
    Some(base + 1 + offset)
}

/// Read a block element's `block_id` attribute.
fn block_id_of<T: ReadTxn>(txn: &T, element: &XmlElementRef) -> Option<String> {
    element
        .attributes(txn)
        .find_map(|(key, value)| match value {
            Out::Any(Any::String(s)) if key == BLOCK_ID_KEY => Some(s.to_string()),
            _ => None,
        })
}

/// Find the top-level block index carrying `block_id`.
fn block_index_by_id<T: ReadTxn>(txn: &T, root: &XmlFragmentRef, block_id: &str) -> Option<usize> {
    (0..root.len(txn)).find(|&i| {
        matches!(root.get(txn, i), Some(XmlOut::Element(e)) if block_id_of(txn, &e).as_deref() == Some(block_id))
    }).map(|i| i as usize)
}
