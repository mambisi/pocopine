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

/// Version of Pine's yrs document/step protocol carried by collab hello frames.
pub const PINE_COLLAB_PROTOCOL_VERSION: u16 = 1;

/// Derive the generic collab compatibility identity from a Pine runtime.
///
/// Keeping this derivation here prevents callers from pairing one runtime's
/// [`pine_richtext::runtime::EditorRuntime::schema`] with another runtime's
/// fingerprint when they construct a client or server.
pub fn runtime_compatibility(
    runtime: &pine_richtext::runtime::EditorRuntime,
) -> pocopine_collab::CompatibilityIdentity {
    pocopine_collab::CompatibilityIdentity::new(
        PINE_COLLAB_PROTOCOL_VERSION,
        runtime.wire_fingerprint(),
    )
    .expect("EditorRuntime wire fingerprints are canonical SHA-256 hex")
}

// The browser collab client. Compiled on wasm32 (the real target) and under
// `test` (so the sync driver is host-tested) — but NOT in a plain host build,
// to keep the codec's host deps lean.
#[cfg(any(target_arch = "wasm32", test))]
mod client;
#[cfg(target_arch = "wasm32")]
pub use client::{CollabConnection, CollabSyncClient, SyncOutcome, random_client_id};

use std::collections::HashMap;
use std::sync::Arc;

use pine_richtext::model::{Attrs, Fragment, Mark, Node, Schema};
use pine_richtext::{RichTextError, RichTextResult, WireNode};
use serde_json::Value;
use yrs::types::Attrs as YAttrs;
use yrs::types::xml::{
    Xml, XmlElementPrelim, XmlElementRef, XmlFragment, XmlFragmentRef, XmlOut, XmlTextPrelim,
    XmlTextRef,
};
use yrs::{Any, Out, ReadTxn, Text, TransactionMut};

/// Reserved key carrying an inline atom's node type inside its embed map.
const EMBED_TYPE_KEY: &str = "$type";

/// Reserved semantic-node version metadata. This key is never exposed through
/// a Pine node's user attr map.
pub const PINE_VERSION_KEY: &str = "$pine_version";

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
    if let Some(version) = node.version() {
        element.insert_attribute(txn, PINE_VERSION_KEY, Any::BigInt(i64::from(version)));
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
    if let Some(version) = node.version() {
        map.insert(
            PINE_VERSION_KEY.to_string(),
            Any::BigInt(i64::from(version)),
        );
    }
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
        let path = format!("$.content[{i}]");
        match root.get(txn, i) {
            Some(XmlOut::Element(element)) => {
                blocks.push(read_block_wire(txn, &element, schema, &path)?);
            }
            Some(other) => {
                return Err(wire_structure_error(
                    path,
                    format!(
                        "document root children must be XmlElement blocks; found {}",
                        xml_out_kind(&other)
                    ),
                ));
            }
            None => {
                return Err(wire_structure_error(path, "document root child is missing"));
            }
        }
    }
    let mut document = WireNode::new(schema.top_node_name(), None);
    document.content = blocks;
    schema.materialize_wire_node(document)
}

fn read_block_wire(
    txn: &impl ReadTxn,
    element: &XmlElementRef,
    schema: &Schema,
    path: &str,
) -> RichTextResult<WireNode> {
    let tag = element.tag().to_string();
    let (attrs, version) = read_attrs(txn, element, &tag, path)?;
    let node_type = schema.node_type(&tag).map_err(|error| {
        wire_structure_error(path, format!("invalid semantic node type `{tag}`: {error}"))
    })?;

    let content = if node_type.inline_content(schema) {
        if element.len(txn) == 0 {
            return Err(wire_structure_error(
                format!("{path}.content"),
                format!("inline-content node `{tag}` is missing its canonical XmlText container"),
            ));
        }
        let xtext = match element.get(txn, 0) {
            Some(XmlOut::Text(xtext)) => xtext,
            Some(other) => {
                return Err(wire_structure_error(
                    format!("{path}.content[0]"),
                    format!(
                        "inline-content node `{tag}` requires one XmlText child; found {}",
                        xml_out_kind(&other)
                    ),
                ));
            }
            None => {
                return Err(wire_structure_error(
                    format!("{path}.content[0]"),
                    format!("inline-content node `{tag}` has a missing child"),
                ));
            }
        };
        if element.len(txn) != 1 {
            return Err(wire_structure_error(
                format!("{path}.content[1]"),
                format!(
                    "inline-content node `{tag}` must contain exactly one XmlText child; found {} children",
                    element.len(txn)
                ),
            ));
        }
        read_inline_wire(txn, &xtext, schema, path)?
    } else {
        let mut blocks = Vec::new();
        for i in 0..element.len(txn) {
            let child_path = format!("{path}.content[{i}]");
            match element.get(txn, i) {
                Some(XmlOut::Element(child)) => {
                    blocks.push(read_block_wire(txn, &child, schema, &child_path)?);
                }
                Some(other) => {
                    return Err(wire_structure_error(
                        child_path,
                        format!(
                            "block-content node `{tag}` requires XmlElement children; found {}",
                            xml_out_kind(&other)
                        ),
                    ));
                }
                None => {
                    return Err(wire_structure_error(
                        child_path,
                        format!("block-content node `{tag}` has a missing child"),
                    ));
                }
            }
        }
        blocks
    };

    Ok(WireNode {
        name: tag,
        version,
        attrs,
        marks: Vec::new(),
        content,
        text: None,
        leaf: node_type.is_leaf(),
    })
}

fn read_attrs(
    txn: &impl ReadTxn,
    element: &XmlElementRef,
    node_type: &str,
    path: &str,
) -> RichTextResult<(Attrs, Option<u32>)> {
    let mut attrs = Attrs::new();
    let mut version = None;
    for (key, value) in element.attributes(txn) {
        if key == BLOCK_ID_KEY {
            continue; // yrs metadata, not a pine-richtext attr
        }
        if key == PINE_VERSION_KEY {
            version = Some(read_version_any(
                &value,
                node_type,
                &format!("{path}.version"),
            )?);
            continue;
        }
        if key.starts_with("$pine_") {
            return Err(RichTextError::WireNode {
                path: format!("{path}.attrs.{key}"),
                message: format!("unknown reserved Pine metadata key `{key}`"),
            });
        }
        match value {
            Out::Any(any) => {
                attrs.insert(key.to_string(), any_to_value(&any));
            }
            other => {
                return Err(wire_structure_error(
                    format!("{path}.attrs.{key}"),
                    format!(
                        "node attribute values must be Any scalars/collections; found {}",
                        out_kind(&other)
                    ),
                ));
            }
        }
    }
    Ok((attrs, version))
}

fn read_inline_wire(
    txn: &impl ReadTxn,
    xtext: &XmlTextRef,
    schema: &Schema,
    parent_path: &str,
) -> RichTextResult<Vec<WireNode>> {
    let mut nodes = Vec::new();
    // The closure annotates each run with change info; we don't use it.
    for (index, diff) in xtext.diff(txn, |change| change).into_iter().enumerate() {
        let path = format!("{parent_path}.content[{index}]");
        let marks = match diff.attributes {
            Some(attrs) => yattrs_to_marks(&attrs, schema, &path)?,
            None => Vec::new(),
        };
        match diff.insert {
            Out::Any(Any::String(text)) => nodes.push(WireNode {
                name: "text".to_string(),
                version: None,
                attrs: Attrs::new(),
                marks,
                content: Vec::new(),
                text: Some(text.to_string()),
                leaf: false,
            }),
            Out::Any(any) => nodes.push(any_to_atom_wire(&any, schema, &path)?),
            other => {
                return Err(wire_structure_error(
                    path,
                    format!(
                        "inline runs must be text or atom embeds encoded as Any; found {}",
                        out_kind(&other)
                    ),
                ));
            }
        }
    }
    Ok(nodes)
}

fn yattrs_to_marks(attrs: &YAttrs, schema: &Schema, path: &str) -> RichTextResult<Vec<Mark>> {
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
        marks.push(schema.mark(name.to_string(), mark_attrs).map_err(|error| {
            wire_structure_error(
                format!("{path}.marks.{name}"),
                format!("invalid mark `{name}`: {error}"),
            )
        })?);
    }
    // Deterministic order on both peers (rank/dedupe).
    schema
        .normalize_marks(marks)
        .map_err(|error| wire_structure_error(format!("{path}.marks"), error.to_string()))
}

fn any_to_atom_wire(any: &Any, schema: &Schema, path: &str) -> RichTextResult<WireNode> {
    let map = match any {
        Any::Map(map) => map,
        _ => {
            return Err(wire_structure_error(
                path,
                format!(
                    "inline atom embed must be a map with string `{EMBED_TYPE_KEY}`; found {}",
                    any_kind(any)
                ),
            ));
        }
    };
    let tag = match map.get(EMBED_TYPE_KEY) {
        Some(Any::String(s)) => s.to_string(),
        Some(other) => {
            return Err(wire_structure_error(
                format!("{path}.{EMBED_TYPE_KEY}"),
                format!(
                    "inline atom `{EMBED_TYPE_KEY}` must be a string; found {}",
                    any_kind(other)
                ),
            ));
        }
        None => {
            return Err(wire_structure_error(
                format!("{path}.{EMBED_TYPE_KEY}"),
                format!("inline atom embed is missing required `{EMBED_TYPE_KEY}`"),
            ));
        }
    };
    let version = map
        .get(PINE_VERSION_KEY)
        .map(|value| read_version(value, &tag, &format!("{path}.version")))
        .transpose()?;
    let attrs = map
        .iter()
        .filter(|(k, _)| k.as_str() != EMBED_TYPE_KEY && k.as_str() != PINE_VERSION_KEY)
        .map(|(k, v)| (k.clone(), any_to_value(v)))
        .collect();
    if map
        .keys()
        .any(|key| key.starts_with("$pine_") && key != PINE_VERSION_KEY)
    {
        return Err(RichTextError::WireNode {
            path: format!("{path}.attrs"),
            message: "inline atom contains unknown reserved Pine metadata".to_string(),
        });
    }
    let node_type = schema.node_type(&tag).map_err(|error| {
        wire_structure_error(path, format!("invalid inline atom type `{tag}`: {error}"))
    })?;
    Ok(WireNode {
        name: tag,
        version,
        attrs,
        marks: Vec::new(),
        content: Vec::new(),
        text: None,
        leaf: node_type.is_leaf(),
    })
}

fn read_version_any(value: &Out, node_type: &str, path: &str) -> RichTextResult<u32> {
    match value {
        Out::Any(value) => read_version(value, node_type, path),
        _ => Err(invalid_version(node_type, path)),
    }
}

fn read_version(value: &Any, node_type: &str, path: &str) -> RichTextResult<u32> {
    let version = match value {
        Any::BigInt(value) => u32::try_from(*value).ok(),
        Any::Number(value) if value.fract() == 0.0 => u32::try_from(*value as i64).ok(),
        _ => None,
    };
    version
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_version(node_type, path))
}

fn invalid_version(node_type: &str, path: &str) -> RichTextError {
    RichTextError::WireNode {
        path: path.to_string(),
        message: format!("node `{node_type}` metadata `{PINE_VERSION_KEY}` must be a positive u32"),
    }
}

fn wire_structure_error(path: impl Into<String>, message: impl Into<String>) -> RichTextError {
    RichTextError::WireNode {
        path: path.into(),
        message: message.into(),
    }
}

fn xml_out_kind(value: &XmlOut) -> &'static str {
    match value {
        XmlOut::Element(_) => "XmlElement",
        XmlOut::Fragment(_) => "XmlFragment",
        XmlOut::Text(_) => "XmlText",
    }
}

fn out_kind(value: &Out) -> &'static str {
    match value {
        Out::Any(_) => "Any",
        Out::YText(_) => "YText",
        Out::YArray(_) => "YArray",
        Out::YMap(_) => "YMap",
        Out::YXmlElement(_) => "YXmlElement",
        Out::YXmlFragment(_) => "YXmlFragment",
        Out::YXmlText(_) => "YXmlText",
        Out::YDoc(_) => "YDoc",
        _ => "unsupported shared type",
    }
}

fn any_kind(value: &Any) -> &'static str {
    match value {
        Any::Null => "null",
        Any::Undefined => "undefined",
        Any::Bool(_) => "boolean",
        Any::Number(_) => "number",
        Any::BigInt(_) => "bigint",
        Any::String(_) => "string",
        Any::Buffer(_) => "buffer",
        Any::Array(_) => "array",
        Any::Map(_) => "map",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pine_richtext::model::{NodeSpec, Schema};
    use pine_richtext::{
        NodeMigration, NodeMigrationError, RichTextNodeAttrs, RichTextNodeType, TypedNodeSpec,
        schema_basic,
    };
    use serde::{Deserialize, Serialize};
    use yrs::{Doc, GetString, MapPrelim, Transact};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
    struct VersionedChipAttrs {
        label: String,
    }

    struct VersionedChipNode;

    fn migrate_chip_v1(mut wire: WireNode) -> Result<WireNode, NodeMigrationError> {
        let Some(label) = wire.attrs.remove("name") else {
            return Err(NodeMigrationError::new("missing legacy `name` attr"));
        };
        wire.attrs.insert("label".to_string(), label);
        wire.version = Some(2);
        Ok(wire)
    }

    static CHIP_MIGRATIONS: [NodeMigration; 1] = [NodeMigration::new(1, 2, migrate_chip_v1)];

    impl RichTextNodeType for VersionedChipNode {
        const NAME: &'static str = "versioned_chip";
        const VERSION: u32 = 2;
        type Attrs = VersionedChipAttrs;

        fn spec() -> NodeSpec {
            NodeSpec::new(Self::NAME)
                .group("inline")
                .inline()
                .atom()
                .required_attr("label")
        }

        fn migrations() -> &'static [NodeMigration] {
            &CHIP_MIGRATIONS
        }
    }

    fn versioned_schema() -> Schema {
        Schema::builder()
            .node(NodeSpec::new("doc").content("paragraph+"))
            .node(NodeSpec::new("paragraph").group("block").content("inline*"))
            .node(NodeSpec::new("text").group("inline").inline())
            .typed_node(TypedNodeSpec::of::<VersionedChipNode>())
            .finish()
            .unwrap()
    }

    /// A deterministic, RNG-free block-id source for tests.
    fn block_ids() -> impl FnMut() -> String {
        let mut n = 0u64;
        move || {
            n += 1;
            format!("b{n}")
        }
    }

    fn assert_wire_error(error: RichTextError, expected_path: &str, message: &str) {
        match error {
            RichTextError::WireNode {
                path,
                message: actual,
            } => {
                assert_eq!(path, expected_path);
                assert!(
                    actual.contains(message),
                    "expected {actual:?} to contain {message:?}"
                );
            }
            other => panic!("expected structured WireNode error, got {other:?}"),
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
    fn inline_typed_node_round_trip_preserves_reserved_version_metadata() {
        let schema = versioned_schema();
        let mut attrs = Attrs::new();
        attrs.insert("label".to_string(), Value::String("alpha".to_string()));
        let chip = schema
            .node(VersionedChipNode::NAME, attrs, Fragment::empty())
            .unwrap();
        let paragraph = schema
            .node("paragraph", Attrs::new(), Fragment::from(vec![chip]))
            .unwrap();
        let doc = schema
            .node("doc", Attrs::new(), Fragment::from(vec![paragraph]))
            .unwrap();

        let source = Doc::with_client_id(91);
        let root = source.get_or_insert_xml_fragment("doc");
        {
            let mut txn = source.transact_mut();
            encode_doc(&mut txn, &root, &doc, &schema, &mut block_ids()).unwrap();
        }

        let txn = source.transact();
        let paragraph = match root.get(&txn, 0) {
            Some(XmlOut::Element(element)) => element,
            _ => panic!("paragraph element"),
        };
        let text = match paragraph.get(&txn, 0) {
            Some(XmlOut::Text(text)) => text,
            _ => panic!("inline text container"),
        };
        let embed = text
            .diff(&txn, |change| change)
            .into_iter()
            .find_map(|diff| match diff.insert {
                Out::Any(Any::Map(map)) => Some(map),
                _ => None,
            })
            .expect("typed atom embed");
        assert_eq!(embed.get(PINE_VERSION_KEY), Some(&Any::BigInt(2)));

        let decoded = decode_doc(&txn, &root, &schema).unwrap();
        assert_eq!(decoded, doc);
        assert_eq!(
            decoded
                .content()
                .child(0)
                .unwrap()
                .content()
                .child(0)
                .unwrap()
                .version(),
            Some(2)
        );
    }

    #[test]
    fn collab_decode_routes_legacy_version_through_wire_migration() {
        let schema = versioned_schema();
        let source = Doc::with_client_id(92);
        let root = source.get_or_insert_xml_fragment("doc");
        {
            let mut txn = source.transact_mut();
            let paragraph = root.push_back(&mut txn, XmlElementPrelim::empty("paragraph"));
            paragraph.insert_attribute(&mut txn, BLOCK_ID_KEY, Any::String(Arc::from("b1")));
            let text = paragraph.push_back(&mut txn, XmlTextPrelim::new(""));
            let mut legacy = HashMap::new();
            legacy.insert(
                EMBED_TYPE_KEY.to_string(),
                Any::String(Arc::from(VersionedChipNode::NAME)),
            );
            legacy.insert(PINE_VERSION_KEY.to_string(), Any::BigInt(1));
            legacy.insert("name".to_string(), Any::String(Arc::from("legacy")));
            text.insert_embed(&mut txn, 0, Any::Map(Arc::new(legacy)));
        }

        let decoded = decode_doc(&source.transact(), &root, &schema).unwrap();
        let chip = decoded
            .content()
            .child(0)
            .unwrap()
            .content()
            .child(0)
            .unwrap();
        assert_eq!(chip.version(), Some(2));
        assert_eq!(
            chip.attrs().get("label"),
            Some(&Value::String("legacy".into()))
        );
        assert!(!chip.attrs().contains_key(PINE_VERSION_KEY));
        assert!(!chip.attrs().contains_key("name"));
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
    fn canonical_empty_inline_block_retains_one_empty_xml_text_child() {
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![schema_basic::paragraph(vec![]).unwrap()]).unwrap();
        let source = Doc::with_client_id(93);
        let root = source.get_or_insert_xml_fragment("doc");
        {
            let mut txn = source.transact_mut();
            encode_doc(&mut txn, &root, &doc, &schema, &mut block_ids()).unwrap();
        }

        let txn = source.transact();
        let Some(XmlOut::Element(paragraph)) = root.get(&txn, 0) else {
            panic!("canonical paragraph element")
        };
        assert_eq!(paragraph.len(&txn), 1);
        let Some(XmlOut::Text(text)) = paragraph.get(&txn, 0) else {
            panic!("canonical empty XmlText container")
        };
        assert_eq!(text.len(&txn), 0);
        assert_eq!(decode_doc(&txn, &root, &schema).unwrap(), doc);
    }

    #[test]
    fn malformed_inline_embeds_fail_with_exact_wire_paths() {
        let schema = schema_basic::schema();

        let non_map = Doc::with_client_id(94);
        let root = non_map.get_or_insert_xml_fragment("doc");
        {
            let mut txn = non_map.transact_mut();
            let paragraph = root.push_back(&mut txn, XmlElementPrelim::empty("paragraph"));
            let text = paragraph.push_back(&mut txn, XmlTextPrelim::new(""));
            text.insert_embed(&mut txn, 0, Any::Bool(true));
        }
        assert_wire_error(
            decode_doc(&non_map.transact(), &root, &schema).unwrap_err(),
            "$.content[0].content[0]",
            "must be a map",
        );

        let missing_type = Doc::with_client_id(95);
        let root = missing_type.get_or_insert_xml_fragment("doc");
        {
            let mut txn = missing_type.transact_mut();
            let paragraph = root.push_back(&mut txn, XmlElementPrelim::empty("paragraph"));
            let text = paragraph.push_back(&mut txn, XmlTextPrelim::new(""));
            let mut embed = HashMap::new();
            embed.insert("src".to_string(), Any::String(Arc::from("/image.png")));
            text.insert_embed(&mut txn, 0, Any::Map(Arc::new(embed)));
        }
        assert_wire_error(
            decode_doc(&missing_type.transact(), &root, &schema).unwrap_err(),
            "$.content[0].content[0].$type",
            "missing required `$type`",
        );
    }

    #[test]
    fn unexpected_shared_inline_insert_is_not_silently_dropped() {
        let schema = schema_basic::schema();
        let source = Doc::with_client_id(96);
        let root = source.get_or_insert_xml_fragment("doc");
        {
            let mut txn = source.transact_mut();
            let paragraph = root.push_back(&mut txn, XmlElementPrelim::empty("paragraph"));
            let text = paragraph.push_back(&mut txn, XmlTextPrelim::new(""));
            text.insert_embed(&mut txn, 0, MapPrelim::default());
        }

        assert_wire_error(
            decode_doc(&source.transact(), &root, &schema).unwrap_err(),
            "$.content[0].content[0]",
            "found YMap",
        );
    }

    #[test]
    fn unexpected_or_missing_xml_structure_is_not_silently_dropped() {
        let schema = schema_basic::schema();

        let root_text = Doc::with_client_id(97);
        let root = root_text.get_or_insert_xml_fragment("doc");
        {
            let mut txn = root_text.transact_mut();
            root.push_back(&mut txn, XmlTextPrelim::new("rogue"));
        }
        assert_wire_error(
            decode_doc(&root_text.transact(), &root, &schema).unwrap_err(),
            "$.content[0]",
            "root children must be XmlElement",
        );

        let block_text = Doc::with_client_id(98);
        let root = block_text.get_or_insert_xml_fragment("doc");
        {
            let mut txn = block_text.transact_mut();
            let list = root.push_back(&mut txn, XmlElementPrelim::empty("bullet_list"));
            list.push_back(&mut txn, XmlTextPrelim::new("rogue"));
        }
        assert_wire_error(
            decode_doc(&block_text.transact(), &root, &schema).unwrap_err(),
            "$.content[0].content[0]",
            "requires XmlElement children",
        );

        let missing_inline = Doc::with_client_id(99);
        let root = missing_inline.get_or_insert_xml_fragment("doc");
        {
            let mut txn = missing_inline.transact_mut();
            root.push_back(&mut txn, XmlElementPrelim::empty("paragraph"));
        }
        assert_wire_error(
            decode_doc(&missing_inline.transact(), &root, &schema).unwrap_err(),
            "$.content[0].content",
            "missing its canonical XmlText container",
        );
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
