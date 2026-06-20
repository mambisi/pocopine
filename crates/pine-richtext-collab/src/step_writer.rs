//! Incremental `Step` → yrs writer (Phase 5 fine-diff, v1).
//!
//! The coarse [`CollabEditor::set_document`](crate::CollabEditor::set_document)
//! re-encodes the whole document on every edit, tombstoning the tree — which
//! makes two live cursors fight and destroys `StickyIndex` anchors. This module
//! translates a pine-richtext [`Step`] directly into *targeted* yrs mutations, so
//! a local edit touches only the affected block's `XmlText` and concurrent edits
//! to different spans converge without loss.
//!
//! ## v1 scope
//!
//! Only the common **in-block inline edits** are applied finely:
//! - a [`Step::Replace`] whose slice is inline-only (`open_start == open_end == 0`)
//!   and whose range stays within one flat textblock — i.e. typing, deleting,
//!   pasting inline text/atoms;
//! - [`Step::AddMark`] / [`Step::RemoveMark`] within one flat textblock.
//!
//! Everything else (block split/merge/move, list nesting, node attrs) returns
//! `Ok(false)`, and the caller falls back to a coarse rebuild. Those structural
//! cases carry permanent concurrent-loss anyway (schema-A, design doc §6), so the
//! coarse fallback costs nothing in convergence — only in granularity.

use pine_richtext::RichTextResult;
use pine_richtext::model::{Node, Schema, Slice};
use pine_richtext::transform::Step;
use yrs::types::Attrs as YAttrs;
use yrs::types::xml::{XmlFragmentRef, XmlOut, XmlTextRef};
use yrs::{Any, Text, TransactionMut, XmlFragment};

use crate::{insert_inline, marks_to_yattrs};

/// Try to apply `step` as a fine-grained in-block mutation against `root` (whose
/// current decode is `before`, used to resolve the step's positions). Returns
/// `Ok(true)` if applied, `Ok(false)` if `step` isn't finely supported (the
/// caller then rebuilds the whole document coarsely).
pub(crate) fn try_apply_fine(
    txn: &mut TransactionMut,
    root: &XmlFragmentRef,
    before: &Node,
    schema: &Schema,
    step: &Step,
) -> RichTextResult<bool> {
    match step {
        Step::Replace(s) if is_inline_only(&s.slice, schema) => {
            let (Some((from_block, from_off)), Some((to_block, to_off))) = (
                flat_block_offset(before, schema, s.from),
                flat_block_offset(before, schema, s.to),
            ) else {
                return Ok(false);
            };
            // A range that spans block boundaries is structural — coarse path.
            if from_block != to_block {
                return Ok(false);
            }
            let Some(xtext) = block_xtext(txn, root, from_block) else {
                return Ok(false);
            };
            if to_off > from_off {
                xtext.remove_range(txn, from_off, to_off - from_off);
            }
            insert_inline(txn, &xtext, from_off, &s.slice.content);
            Ok(true)
        }
        Step::AddMark(s) | Step::RemoveMark(s) => {
            let add = matches!(step, Step::AddMark(_));
            let (Some((from_block, from_off)), Some((to_block, to_off))) = (
                flat_block_offset(before, schema, s.from),
                flat_block_offset(before, schema, s.to),
            ) else {
                return Ok(false);
            };
            if from_block != to_block || to_off <= from_off {
                return Ok(false);
            }
            let Some(xtext) = block_xtext(txn, root, from_block) else {
                return Ok(false);
            };
            let attrs = if add {
                marks_to_yattrs(std::slice::from_ref(&s.mark))
            } else {
                // Removing a mark: format the range with the key set to Null, the
                // yrs idiom for clearing a format attribute over a span.
                YAttrs::from([(s.mark.type_name().into(), Any::Null)])
            };
            xtext.format(txn, from_off, to_off - from_off, attrs);
            Ok(true)
        }
        // Replace with an open (block-crossing) slice, ReplaceAround, node marks,
        // attrs, doc attrs — all structural; the caller rebuilds coarsely.
        _ => Ok(false),
    }
}

/// True when a slice is purely inline content with no open block boundaries — the
/// shape of a plain text/atom insertion or replacement.
fn is_inline_only(slice: &Slice, schema: &Schema) -> bool {
    slice.open_start == 0
        && slice.open_end == 0
        && slice
            .content
            .iter()
            .all(|node| node_is_inline(node, schema))
}

/// Whether `node` is inline content (a text node or an inline atom per schema).
fn node_is_inline(node: &Node, schema: &Schema) -> bool {
    node.is_text()
        || schema
            .node_type(node.type_name())
            .map(|ty| ty.is_inline())
            .unwrap_or(false)
}

/// Resolve an absolute model position to `(top-level block index, inline **byte**
/// offset within that block's XmlText)`, but ONLY when the position lands strictly
/// inside a **flat textblock** (a top-level block that takes inline content —
/// paragraph, heading). Returns `None` for positions at block boundaries or inside
/// nested block structures (lists, blockquotes). Shared with the caret codec.
///
/// The returned offset is in UTF-8 bytes (yrs' `XmlText` unit), converted from the
/// model's char offset via [`char_to_byte`](crate::char_to_byte) — so an edit in
/// text containing multi-byte characters lands at the right yrs position.
pub(crate) fn flat_block_offset(doc: &Node, schema: &Schema, pos: usize) -> Option<(usize, u32)> {
    let mut base = 0usize; // model position just before block `i`
    for i in 0..doc.child_count() {
        let block = doc.child(i)?;
        let size = block.node_size();
        // Block `i` spans [base, base+size); its inline content spans the open
        // interval (base, base+size), i.e. char offsets [base+1, base+size-1].
        if pos > base && pos < base + size {
            let node_type = schema.node_type(block.type_name()).ok()?;
            if !node_type.inline_content(schema) {
                return None; // a nested block-content block — coarse path
            }
            let char_off = pos - base - 1;
            return Some((i, crate::char_to_byte(block.content(), char_off)));
        }
        base += size;
    }
    None
}

/// The `XmlText` carrying the inline content of the `i`-th top-level block.
fn block_xtext(txn: &TransactionMut, root: &XmlFragmentRef, i: usize) -> Option<XmlTextRef> {
    match root.get(txn, i as u32)? {
        XmlOut::Element(element) => match element.get(txn, 0)? {
            XmlOut::Text(xtext) => Some(xtext),
            _ => None,
        },
        _ => None,
    }
}
