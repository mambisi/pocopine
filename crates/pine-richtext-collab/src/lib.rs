//! # pine-richtext-collab
//!
//! The codec between the [`pine_richtext`] ProseMirror document model and a
//! `yrs` CRDT, for live-block collaboration (RFC-073 live blocks). It is one
//! Rust crate compiled twice — host (SSR / persistence / server-authored docs)
//! and `wasm32` (the browser editor) — so the same mapping is authoritative on
//! both ends.
//!
//! ## Schema (the decided layout)
//!
//! A document is a y-prosemirror-isomorphic nested `XmlFragment` tree:
//! - a block `Node` → an `XmlElement`, tag = the node type, element attributes =
//!   the node's attrs;
//! - a block with inline content holds **exactly one `XmlText`** carrying its
//!   text, with marks as per-range format attributes and inline atoms
//!   (`image` / `hard_break`) as `insert_embed` embeds **inside** that text;
//! - a block with block content holds only `XmlElement` children.
//!
//! See `docs/internal/collab-live-blocks-schema.md`.
//!
//! This first cut is the bidirectional codec — `encode_doc` (Node → CRDT) and
//! `decode_doc` (CRDT → Node) — proven by the `Node == Node` round-trip. The
//! stable per-block `block_id` (yrs metadata, invisible to the `Node`) and the
//! live editor binding are follow-ups.

mod binder;
mod caret;
mod step_writer;
pub use binder::{BindError, CollabEditor};
pub use caret::StickyPoint;

// The browser collab client. Compiled on wasm32 (the real target) and under
// `test` (so the sync driver is host-tested) — but NOT in a plain host build,
// to keep the codec's host deps lean.
#[cfg(any(target_arch = "wasm32", test))]
mod client;
#[cfg(target_arch = "wasm32")]
pub use client::{CollabConnection, CollabSyncClient, SyncOutcome};

use std::collections::HashMap;
use std::sync::Arc;

use pine_richtext::RichTextResult;
use pine_richtext::model::{Attrs, Fragment, Mark, Node, Schema};
use serde_json::Value;
use yrs::types::Attrs as YAttrs;
use yrs::types::xml::{
    Xml, XmlElementPrelim, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim,
    XmlTextRef,
};
use yrs::{Any, Out, ReadTxn, Text, TransactionMut};

/// Reserved key carrying an inline atom's node type inside its embed map.
const EMBED_TYPE_KEY: &str = "$type";

/// Element attribute holding a block's stable identity. yrs metadata — it is
/// NOT a pine-richtext attr, so the reader strips it (a `Node` never carries it).
/// v1: a fresh id per block-creating write; identity / divergence-detection /
/// migration seam, not move-reconciliation.
pub const BLOCK_ID_KEY: &str = "block_id";

// ---------------------------------------------------------------------------
// attr-value codec: serde_json::Value <-> yrs Any  (a bijection on the PM domain)
// ---------------------------------------------------------------------------

/// Encode a PM attribute value as a yrs `Any`. Integral JSON numbers become
/// `BigInt` (and decode back to integers), so small ints like `heading.level`
/// round-trip exactly.
pub fn value_to_any(value: &Value) -> Any {
    match value {
        Value::Null => Any::Null,
        Value::Bool(b) => Any::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => Any::BigInt(i),
            None => Any::Number(n.as_f64().unwrap_or(0.0)),
        },
        Value::String(s) => Any::String(Arc::from(s.as_str())),
        Value::Array(items) => Any::Array(items.iter().map(value_to_any).collect()),
        Value::Object(map) => Any::Map(Arc::new(
            map.iter()
                .map(|(k, v)| (k.clone(), value_to_any(v)))
                .collect(),
        )),
    }
}

/// Decode a yrs `Any` back to a PM attribute value (inverse of [`value_to_any`]
/// on the domain PM actually uses).
pub fn any_to_value(any: &Any) -> Value {
    match any {
        Any::Null | Any::Undefined => Value::Null,
        Any::Bool(b) => Value::Bool(*b),
        Any::BigInt(i) => Value::Number((*i).into()),
        Any::Number(f) => serde_json::Number::from_f64(*f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Any::String(s) => Value::String(s.to_string()),
        Any::Array(items) => Value::Array(items.iter().map(any_to_value).collect()),
        Any::Map(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), any_to_value(v)))
                .collect(),
        ),
        Any::Buffer(_) => Value::Null, // not in the PM attr domain
    }
}

fn pm_attrs_to_any_map(attrs: &Attrs) -> Any {
    Any::Map(Arc::new(
        attrs
            .iter()
            .map(|(k, v)| (k.clone(), value_to_any(v)))
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// write: Node -> yrs XmlFragment
// ---------------------------------------------------------------------------

/// Encode a top-level document `Node` into the root `XmlFragment` (one
/// `XmlElement` per top-level block). `next_block_id` mints a fresh
/// [`BLOCK_ID_KEY`] for every block element written (the caller owns the policy
/// — typically `"{client_id}-{counter}"`, RNG-free for wasm).
pub fn encode_doc<F: FnMut() -> String>(
    txn: &mut TransactionMut,
    root: &XmlFragmentRef,
    doc: &Node,
    schema: &Schema,
    next_block_id: &mut F,
) -> RichTextResult<()> {
    for block in doc.content().iter() {
        write_block(txn, root, block, schema, next_block_id)?;
    }
    Ok(())
}

fn write_block<P: XmlFragment, F: FnMut() -> String>(
    txn: &mut TransactionMut,
    parent: &P,
    node: &Node,
    schema: &Schema,
    next_block_id: &mut F,
) -> RichTextResult<()> {
    let element = parent.push_back(txn, XmlElementPrelim::empty(node.type_name()));
    for (key, value) in node.attrs() {
        element.insert_attribute(txn, key.as_str(), value_to_any(value));
    }
    element.insert_attribute(
        txn,
        BLOCK_ID_KEY,
        Any::String(Arc::from(next_block_id().as_str())),
    );

    if schema.node_type(node.type_name())?.inline_content(schema) {
        // One XmlText child holds the whole inline run (text + marks + atoms).
        let xtext = element.push_back(txn, XmlTextPrelim::new(""));
        write_inline(txn, &xtext, node.content());
    } else {
        // Block content: recurse into child elements.
        for child in node.content().iter() {
            write_block(txn, &element, child, schema, next_block_id)?;
        }
    }
    Ok(())
}

fn write_inline(txn: &mut TransactionMut, xtext: &XmlTextRef, content: &Fragment) {
    insert_inline(txn, xtext, 0, content);
}

/// Insert a fragment of inline content into `xtext` starting at **UTF-8 byte**
/// index `at`. Inserts all text/embeds first, then applies marks (the two-phase
/// write that keeps a mark from bleeding past its run when the next insert lands
/// on its boundary). Shared by the whole-doc codec and the incremental
/// [`step_writer`].
///
/// yrs `Text`/`XmlText` index everything in UTF-8 bytes (the default
/// `OffsetKind::Bytes`), so every index here is a byte offset — never a char
/// count, which would mis-place edits in text containing multi-byte characters.
/// Callers convert pine's char-based model positions with [`char_to_byte`].
pub(crate) fn insert_inline(
    txn: &mut TransactionMut,
    xtext: &XmlTextRef,
    at: u32,
    content: &Fragment,
) {
    let mut index = at;
    let mut runs: Vec<(u32, u32, &[Mark])> = Vec::new();
    for node in content.iter() {
        if let Some(text) = node.text() {
            xtext.insert(txn, index, text);
            let len = text.len() as u32; // UTF-8 byte length
            if !node.marks().is_empty() {
                runs.push((index, len, node.marks()));
            }
            index += len;
        } else {
            // Inline atom (image / hard_break) → an embed, one yrs position wide.
            xtext.insert_embed(txn, index, atom_to_any(node));
            index += 1;
        }
    }
    for (start, len, marks) in runs {
        xtext.format(txn, start, len, marks_to_yattrs(marks));
    }
}

/// Convert a char offset into a fragment of inline content to the matching
/// UTF-8 **byte** offset (the unit yrs `XmlText` indexes by). Text contributes
/// its bytes; an inline atom is one unit (char and byte alike). Used to map a
/// pine model position into a block's `XmlText`.
pub(crate) fn char_to_byte(content: &Fragment, char_off: usize) -> u32 {
    let mut chars_seen = 0usize;
    let mut bytes = 0u32;
    for node in content.iter() {
        if let Some(text) = node.text() {
            let n = text.chars().count();
            if char_off <= chars_seen + n {
                let within = char_off - chars_seen;
                bytes += text
                    .chars()
                    .take(within)
                    .map(|c| c.len_utf8() as u32)
                    .sum::<u32>();
                return bytes;
            }
            chars_seen += n;
            bytes += text.len() as u32;
        } else {
            if char_off <= chars_seen {
                return bytes;
            }
            chars_seen += 1;
            bytes += 1;
        }
    }
    bytes
}

/// Inverse of [`char_to_byte`]: a UTF-8 byte offset back to a char offset.
pub(crate) fn byte_to_char(content: &Fragment, byte_off: u32) -> usize {
    let mut bytes_seen = 0u32;
    let mut chars = 0usize;
    for node in content.iter() {
        if let Some(text) = node.text() {
            let nbytes = text.len() as u32;
            if byte_off <= bytes_seen + nbytes {
                let within = byte_off - bytes_seen;
                let mut acc = 0u32;
                for (ci, c) in text.chars().enumerate() {
                    if acc >= within {
                        return chars + ci;
                    }
                    acc += c.len_utf8() as u32;
                }
                return chars + text.chars().count();
            }
            bytes_seen += nbytes;
            chars += text.chars().count();
        } else {
            if byte_off <= bytes_seen {
                return chars;
            }
            bytes_seen += 1;
            chars += 1;
        }
    }
    chars
}

pub(crate) fn marks_to_yattrs(marks: &[Mark]) -> YAttrs {
    marks
        .iter()
        .map(|mark| {
            let value = if mark.attrs().is_empty() {
                Any::Bool(true) // boolean marks (em/strong/code): one canonical shape
            } else {
                pm_attrs_to_any_map(mark.attrs()) // e.g. link {href, title}
            };
            (Arc::from(mark.type_name()), value)
        })
        .collect()
}

pub(crate) fn atom_to_any(node: &Node) -> Any {
    let mut map: HashMap<String, Any> = node
        .attrs()
        .iter()
        .map(|(k, v)| (k.clone(), value_to_any(v)))
        .collect();
    map.insert(
        EMBED_TYPE_KEY.to_string(),
        Any::String(Arc::from(node.type_name())),
    );
    Any::Map(Arc::new(map))
}

// ---------------------------------------------------------------------------
// read: yrs XmlFragment -> Node
// ---------------------------------------------------------------------------

/// Decode the root `XmlFragment` back into a top-level document `Node`.
pub fn decode_doc(
    txn: &impl ReadTxn,
    root: &XmlFragmentRef,
    schema: &Schema,
) -> RichTextResult<Node> {
    let mut blocks = Vec::new();
    for i in 0..root.len(txn) {
        if let Some(XmlOut::Element(element)) = root.get(txn, i) {
            blocks.push(read_block(txn, &element, schema)?);
        }
    }
    schema.node(
        schema.top_node_name().to_string(),
        Attrs::new(),
        Fragment::new(blocks),
    )
}

fn read_block(
    txn: &impl ReadTxn,
    element: &XmlElementRef,
    schema: &Schema,
) -> RichTextResult<Node> {
    let tag = element.tag().to_string();
    let attrs = read_attrs(txn, element);

    let content = if schema.node_type(&tag)?.inline_content(schema) {
        match element.get(txn, 0) {
            Some(XmlOut::Text(xtext)) => read_inline(txn, &xtext, schema)?,
            _ => Fragment::new(Vec::new()),
        }
    } else {
        let mut blocks = Vec::new();
        for i in 0..element.len(txn) {
            if let Some(XmlOut::Element(child)) = element.get(txn, i) {
                blocks.push(read_block(txn, &child, schema)?);
            }
        }
        Fragment::new(blocks)
    };

    schema.node(tag, attrs, content)
}

fn read_attrs(txn: &impl ReadTxn, element: &XmlElementRef) -> Attrs {
    let mut attrs = Attrs::new();
    for (key, value) in element.attributes(txn) {
        if key == BLOCK_ID_KEY {
            continue; // yrs metadata, not a pine-richtext attr
        }
        if let Out::Any(any) = value {
            attrs.insert(key.to_string(), any_to_value(&any));
        }
    }
    attrs
}

fn read_inline(
    txn: &impl ReadTxn,
    xtext: &XmlTextRef,
    schema: &Schema,
) -> RichTextResult<Fragment> {
    let mut nodes = Vec::new();
    // The closure annotates each run with change info; we don't use it.
    for diff in xtext.diff(txn, |change| change) {
        let marks = match diff.attributes {
            Some(attrs) => yattrs_to_marks(&attrs, schema)?,
            None => Vec::new(),
        };
        match diff.insert {
            Out::Any(Any::String(text)) => nodes.push(schema.text(text.to_string(), marks)?),
            Out::Any(any) => nodes.push(any_to_atom(&any, schema)?),
            _ => {} // non-Any inserts are not part of this schema
        }
    }
    Ok(Fragment::new(nodes))
}

fn yattrs_to_marks(attrs: &YAttrs, schema: &Schema) -> RichTextResult<Vec<Mark>> {
    let mut marks = Vec::new();
    for (name, value) in attrs.iter() {
        if matches!(value, Any::Null) {
            continue; // a removed mark
        }
        let mark_attrs = match value {
            Any::Map(map) => map
                .iter()
                .map(|(k, v)| (k.clone(), any_to_value(v)))
                .collect(),
            _ => Attrs::new(), // boolean marks
        };
        marks.push(schema.mark(name.to_string(), mark_attrs)?);
    }
    // Deterministic order on both peers (rank/dedupe).
    schema.normalize_marks(marks)
}

fn any_to_atom(any: &Any, schema: &Schema) -> RichTextResult<Node> {
    let map = match any {
        Any::Map(map) => map,
        _ => return schema.text(String::new(), Vec::new()),
    };
    let tag = match map.get(EMBED_TYPE_KEY) {
        Some(Any::String(s)) => s.to_string(),
        _ => return schema.text(String::new(), Vec::new()),
    };
    let attrs = map
        .iter()
        .filter(|(k, _)| k.as_str() != EMBED_TYPE_KEY)
        .map(|(k, v)| (k.clone(), any_to_value(v)))
        .collect();
    schema.node(tag, attrs, Fragment::new(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::schema_basic;
    use yrs::{Doc, GetString, Transact};

    /// A deterministic, RNG-free block-id source for tests.
    fn block_ids() -> impl FnMut() -> String {
        let mut n = 0u64;
        move || {
            n += 1;
            format!("b{n}")
        }
    }

    /// Round-trip a doc Node through the CRDT (including the binary update wire)
    /// and assert it comes back equal — the Phase-1 acceptance gate.
    fn round_trip(doc: &Node) -> Node {
        let schema = schema_basic::schema();

        // Node -> yrs.
        let source = Doc::with_client_id(1);
        let frag = source.get_or_insert_xml_fragment("doc");
        {
            let mut txn = source.transact_mut();
            encode_doc(&mut txn, &frag, doc, &schema, &mut block_ids()).unwrap();
        }

        // ...across the binary update format (the real transport)...
        let update = source
            .transact()
            .encode_state_as_update_v1(&yrs::StateVector::default());
        let target = Doc::with_client_id(2);
        let target_frag = target.get_or_insert_xml_fragment("doc");
        {
            use yrs::updates::decoder::Decode;
            let mut txn = target.transact_mut();
            txn.apply_update(yrs::Update::decode_v1(&update).unwrap())
                .unwrap();
        }

        // yrs -> Node.
        decode_doc(&target.transact(), &target_frag, &schema).unwrap()
    }

    #[test]
    fn paragraph_with_marks_round_trips() {
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![
            schema_basic::heading(2, vec![schema_basic::text("Notes", vec![]).unwrap()]).unwrap(),
            schema_basic::paragraph(vec![
                schema_basic::text("see ", vec![]).unwrap(),
                schema_basic::text("bold", vec![schema_basic::strong().unwrap()]).unwrap(),
                schema_basic::text(" and ", vec![]).unwrap(),
                schema_basic::text("italic", vec![schema_basic::em().unwrap()]).unwrap(),
            ])
            .unwrap(),
        ])
        .unwrap();

        let back = round_trip(&doc);
        assert_eq!(back, doc, "doc must survive Node -> yrs -> Node unchanged");
        let _ = schema.top_node_name();
    }

    #[test]
    fn nested_list_round_trips() {
        let doc = schema_basic::doc(vec![
            schema_basic::bullet_list(vec![
                schema_basic::list_item(vec![
                    schema_basic::paragraph(vec![schema_basic::text("first", vec![]).unwrap()])
                        .unwrap(),
                ])
                .unwrap(),
                schema_basic::list_item(vec![
                    schema_basic::paragraph(vec![schema_basic::text("second", vec![]).unwrap()])
                        .unwrap(),
                ])
                .unwrap(),
            ])
            .unwrap(),
        ])
        .unwrap();

        assert_eq!(round_trip(&doc), doc);
    }

    #[test]
    fn inline_atoms_and_link_mark_round_trip() {
        // Exercises the corrected embed path (image / hard_break as embeds INSIDE
        // the text) and a mark carrying attributes (link {href}).
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![
                schema_basic::text("Click ", vec![]).unwrap(),
                schema_basic::text(
                    "here",
                    vec![schema_basic::link("https://pocopine.dev", None::<&str>).unwrap()],
                )
                .unwrap(),
                schema_basic::text(" ", vec![]).unwrap(),
                schema_basic::image("/logo.png", Some("logo"), None::<&str>).unwrap(),
                schema_basic::hard_break().unwrap(),
                schema_basic::text("done", vec![]).unwrap(),
            ])
            .unwrap(),
        ])
        .unwrap();
        assert_eq!(round_trip(&doc), doc);
    }

    #[test]
    fn empty_paragraph_round_trips() {
        let doc = schema_basic::doc(vec![schema_basic::paragraph(vec![]).unwrap()]).unwrap();
        assert_eq!(round_trip(&doc), doc);
    }

    #[test]
    fn yrs_text_run_reads_back_text() {
        // guards the read path independently of equality: the XmlText is built
        // and `get_string` sees the text.
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![schema_basic::text("hello", vec![]).unwrap()]).unwrap(),
        ])
        .unwrap();
        let source = Doc::with_client_id(1);
        let frag = source.get_or_insert_xml_fragment("doc");
        let mut txn = source.transact_mut();
        encode_doc(&mut txn, &frag, &doc, &schema, &mut block_ids()).unwrap();
        if let Some(XmlOut::Element(p)) = frag.get(&txn, 0) {
            if let Some(XmlOut::Text(t)) = p.get(&txn, 0) {
                assert_eq!(t.get_string(&txn), "hello");
            } else {
                panic!("paragraph child 0 should be XmlText");
            }
        } else {
            panic!("root child 0 should be a paragraph element");
        }
    }
}
