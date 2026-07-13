//! Translate between DOM positions (`web_sys::Range`) and pine's model
//! [`Selection`](crate::state::Selection).
//!
//! Each rendered block element carries a `data-pos` attribute (set by
//! `crate::render`) with its outer model position. The forward direction
//! walks up from the DOM caret to the nearest tagged ancestor, then
//! accumulates the offset inside that ancestor's text content. The
//! reverse direction looks up a `[data-pos="N"]` element and descends
//! into its content until the target offset is consumed.
//!
//! Inline mark wrappers (em / strong / code / a) don't carry `data-pos`;
//! the bridge treats them as transparent — it recurses into their text
//! children when summing offsets.

use wasm_bindgen::JsCast;
use web_sys::{Element, Node as DomNode};

use crate::model::Node;
use crate::state::Selection;

/// DOM caret position → model content position. Returns `None` when
/// the DOM position can't be located inside `surface`.
///
/// Browser [`web_sys::Range`] offsets into text nodes are **UTF-16
/// code units** (the JS string-length unit). Pine's model counts text
/// in **Unicode scalar values** (`char`s). For BMP-only text the two
/// are equal; for emoji and other non-BMP code points one scalar
/// occupies two code units and a naive `offset as usize` puts the
/// caret in the wrong place. [`utf16_offset_to_char`] does the
/// conversion right at the text-node boundary.
pub fn dom_pos_to_model(surface: &Element, node: &DomNode, offset: u32) -> Option<usize> {
    if !node_inside_surface(surface, node) {
        return None;
    }

    if node.node_type() == DomNode::TEXT_NODE {
        let parent = node.parent_element()?;
        let textblock = nearest_tagged_ancestor(surface, &parent)?;
        let content_start = textblock_content_start(surface, &textblock);
        let inner = text_offset_within(&textblock, node)?;
        let text = node.text_content().unwrap_or_default();
        let char_offset = utf16_offset_to_char(&text, offset);
        return Some(content_start + inner + char_offset);
    }

    // Element node: `offset` is an index into its children. Sum the
    // model size of each child before that index.
    let element: Element = node.clone().dyn_into().ok()?;
    let content_start = textblock_content_start(surface, &element);
    let inner = sibling_size_before(&element, offset as usize);
    Some(content_start + inner)
}

/// Convert a UTF-16 code-unit offset (what [`web_sys::Range`] uses
/// when pointing into a text node) into a Unicode scalar (`char`)
/// offset (what pine's model counts text in). Saturates at the
/// scalar length of `text` when `utf16_offset` is past the end. An
/// offset that lands *inside* a surrogate pair clamps to the boundary
/// BEFORE the pair — browsers never produce such offsets, but a
/// defensive implementation prevents a buggy `Range` from advancing
/// the caret past a code point boundary.
fn utf16_offset_to_char(text: &str, utf16_offset: u32) -> usize {
    let mut utf16_seen: u32 = 0;
    let mut chars_seen: usize = 0;
    for ch in text.chars() {
        let next = utf16_seen.saturating_add(ch.len_utf16() as u32);
        if next > utf16_offset {
            return chars_seen;
        }
        utf16_seen = next;
        chars_seen += 1;
    }
    chars_seen
}

/// Inverse of [`utf16_offset_to_char`]: convert a Unicode scalar
/// (`char`) offset into a UTF-16 code-unit offset suitable for
/// [`web_sys::Range::set_start`] / `set_end`. Saturates at the
/// UTF-16 length of `text` when `char_offset` is past the end.
fn char_offset_to_utf16(text: &str, char_offset: usize) -> u32 {
    text.chars()
        .take(char_offset)
        .map(|ch| ch.len_utf16() as u32)
        .sum()
}

/// Model content position → DOM `(node, offset)` the caller can feed
/// into `Range::set_start` / `set_end`. Returns `None` if the position
/// can't be resolved in the rendered surface.
pub fn model_pos_to_dom(surface: &Element, doc: &Node, pos: usize) -> Option<(DomNode, u32)> {
    walk_doc(surface, doc, pos)
}

/// Build a model [`Selection`] from a DOM anchor/focus pair.
pub fn range_to_selection(
    surface: &Element,
    anchor_node: &DomNode,
    anchor_offset: u32,
    focus_node: &DomNode,
    focus_offset: u32,
) -> Option<Selection> {
    let anchor = dom_pos_to_model(surface, anchor_node, anchor_offset)?;
    let head = dom_pos_to_model(surface, focus_node, focus_offset)?;
    Some(Selection::text_between(anchor, head))
}

// ---------- helpers ----------

fn node_inside_surface(surface: &Element, node: &DomNode) -> bool {
    if same_node(node, surface.as_ref()) {
        return true;
    }
    surface.contains(Some(node))
}

fn same_node(a: &DomNode, b: &DomNode) -> bool {
    a.is_same_node(Some(b))
}

fn data_pos_of(el: &Element) -> Option<usize> {
    el.get_attribute("data-pos")
        .as_deref()
        .and_then(|s| s.parse::<usize>().ok())
}

/// Walk up from `start` (inclusive) to the nearest ancestor inside
/// `surface` that carries a `data-pos` attribute. The `surface`
/// element itself stands in for the doc and is treated as tagged with
/// `data-pos = 0`.
fn nearest_tagged_ancestor(surface: &Element, start: &Element) -> Option<Element> {
    let mut current = Some(start.clone());
    while let Some(el) = current {
        if el.has_attribute("data-pos") {
            return Some(el);
        }
        if same_node(el.as_ref(), surface.as_ref()) {
            return Some(el);
        }
        current = el.parent_element();
    }
    None
}

/// The model position at the start of an element's *content* (one past
/// its open token for a wrapper; itself for the doc surface).
fn textblock_content_start(surface: &Element, el: &Element) -> usize {
    if same_node(el.as_ref(), surface.as_ref()) {
        0
    } else {
        data_pos_of(el).map(|p| p + 1).unwrap_or(0)
    }
}

/// Sum the model-size contribution of every text/inline node inside
/// `textblock` BEFORE `target`. Walks the DOM depth-first:
/// - text nodes → number of chars.
/// - elements with `data-pos` → 1 (inline leaves like img/hr/br).
/// - elements without `data-pos` → recurse into their children
///   (transparent inline mark wrappers).
fn text_offset_within(textblock: &Element, target: &DomNode) -> Option<usize> {
    let mut acc = 0usize;
    if walk_text_offset(textblock.as_ref(), target, &mut acc) {
        Some(acc)
    } else {
        None
    }
}

fn walk_text_offset(parent: &DomNode, target: &DomNode, acc: &mut usize) -> bool {
    let children = parent.child_nodes();
    for i in 0..children.length() {
        let Some(child) = children.item(i) else {
            continue;
        };
        if same_node(&child, target) {
            return true;
        }
        if child.node_type() == DomNode::TEXT_NODE {
            *acc += child.text_content().unwrap_or_default().chars().count();
            continue;
        }
        if let Some(child_el) = child.dyn_ref::<Element>() {
            if child_el.has_attribute("data-pos") {
                *acc += 1;
                continue;
            }
            if walk_text_offset(&child, target, acc) {
                return true;
            }
        }
    }
    false
}

/// Sum the model size of the first `k` children of `parent`. Each
/// child's size is derived from the `data-pos` gap to the NEXT sibling
/// (or, for the last child, from its rendered subtree).
fn sibling_size_before(parent: &Element, k: usize) -> usize {
    let mut acc = 0usize;
    let children = parent.child_nodes();
    let limit = (k as u32).min(children.length());
    for i in 0..limit {
        let Some(child) = children.item(i) else {
            continue;
        };
        if child.node_type() == DomNode::TEXT_NODE {
            acc += child.text_content().unwrap_or_default().chars().count();
            continue;
        }
        let Some(child_el) = child.dyn_ref::<Element>() else {
            continue;
        };
        if let Some(next_pos) = next_sibling_data_pos(child_el)
            && let Some(our_pos) = data_pos_of(child_el)
        {
            acc += next_pos.saturating_sub(our_pos);
            continue;
        }
        if child_el.has_attribute("data-pos") {
            acc += rendered_model_node_size(child_el);
        } else {
            acc += rendered_content_size(child_el);
        }
    }
    acc
}

fn rendered_content_size(parent: &Element) -> usize {
    sibling_size_before(parent, parent.child_nodes().length() as usize)
}

fn rendered_model_node_size(el: &Element) -> usize {
    if rendered_leaf_node(el) {
        1
    } else {
        rendered_content_size(el) + 2
    }
}

fn rendered_leaf_node(el: &Element) -> bool {
    let tag = el.tag_name().to_ascii_lowercase();
    if matches!(tag.as_str(), "br" | "hr" | "img") {
        return true;
    }

    // Typed atom hosts gain component chrome after mount, but that chrome is
    // not editor-model content. `contenteditable=false` is stamped on every
    // typed atom host, so it must continue to count as one model position even
    // when its component renders labels, icons, or buttons inside the host.
    if el.has_attribute("data-pine-node-view")
        && el.get_attribute("contenteditable").as_deref() == Some("false")
    {
        return true;
    }

    // Unknown schema nodes render as `<span data-type="...">`, so
    // even when empty they are still wrappers with open/close model
    // tokens. A custom atom rendered as an empty custom element has no
    // children and no data-type fallback marker, so count it as one
    // model position.
    el.child_nodes().length() == 0 && !el.has_attribute("data-type")
}

fn next_sibling_data_pos(el: &Element) -> Option<usize> {
    // The gap optimization is valid only for an immediately adjacent model
    // element. `next_element_sibling()` skips text and made the first chip at
    // 86 borrow the next chip's position 92, so it appeared six positions wide.
    let sibling = el.next_sibling()?;
    let sibling = sibling.dyn_into::<Element>().ok()?;
    data_pos_of(&sibling)
}

// ---------- model_pos_to_dom ----------

fn walk_doc(surface: &Element, doc: &Node, target: usize) -> Option<(DomNode, u32)> {
    // The doc itself doesn't have a `data-pos` attribute on any element —
    // the surface element implicitly represents it. Use `None` to tag
    // that case so the walker doesn't confuse "this is the doc" with
    // "the first top-level block at outer position 0".
    walk_node(surface, doc, None, target)
}

fn walk_node(
    surface: &Element,
    model_node: &Node,
    base: Option<usize>,
    target: usize,
) -> Option<(DomNode, u32)> {
    if model_node.is_text() {
        return None; // Text positions are handled by the parent.
    }

    let (dom_el, content_start) = match base {
        None => (surface.clone(), 0),
        Some(n) => (find_element_with_data_pos(surface, n)?, n + 1),
    };

    if target < content_start {
        return None;
    }

    let mut pos = content_start;
    let mut child_dom_index = 0u32;
    for child in model_node.content().iter() {
        let child_size = child.node_size();
        let child_end = pos + child_size;

        // Prefer a real adjacent text-node boundary over a parent child offset.
        // Chrome can stick on or skip across parent offsets next to a nested
        // contenteditable=false atom, whereas text-end/text-start is stable.
        if child.is_text() && target >= pos && target <= child_end {
            let chars_in = target - pos;
            let text_node = dom_text_node_at(&dom_el, child_dom_index as usize)?;
            let text = text_node.text_content().unwrap_or_default();
            let utf16_offset = char_offset_to_utf16(&text, chars_in);
            return Some((text_node, utf16_offset));
        }

        if target == pos {
            return Some((dom_el.unchecked_into::<DomNode>(), child_dom_index));
        }

        if target < child_end
            && let Some(found) = walk_node(surface, child, Some(pos), target)
        {
            return Some(found);
        }

        pos = child_end;
        child_dom_index += 1;
    }

    if target == pos {
        return Some((dom_el.unchecked_into::<DomNode>(), child_dom_index));
    }
    None
}

fn find_element_with_data_pos(surface: &Element, pos: usize) -> Option<Element> {
    let selector = format!("[data-pos=\"{}\"]", pos);
    surface.query_selector(&selector).ok().flatten()
}

/// Find the DOM text node at logical text index `i` inside `parent`.
/// Inline mark wrappers (em/strong/code/a) are transparent — we
/// flatten into them.
fn dom_text_node_at(parent: &Element, i: usize) -> Option<DomNode> {
    let mut idx = 0usize;
    walk_for_text(parent.as_ref(), i, &mut idx)
}

fn walk_for_text(parent: &DomNode, target_i: usize, idx: &mut usize) -> Option<DomNode> {
    let children = parent.child_nodes();
    for n in 0..children.length() {
        let child = children.item(n)?;
        if child.node_type() == DomNode::TEXT_NODE {
            if *idx == target_i {
                return Some(child);
            }
            *idx += 1;
            continue;
        }
        if let Some(child_el) = child.dyn_ref::<Element>() {
            if child_el.has_attribute("data-pos") {
                // Inline leaf — not a text node, but it occupies an
                // index. Skip past it.
                if *idx == target_i {
                    return Some(child);
                }
                *idx += 1;
                continue;
            }
            if let Some(found) = walk_for_text(&child, target_i, idx) {
                return Some(found);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{char_offset_to_utf16, utf16_offset_to_char};

    /// 🙂 occupies one Unicode scalar but two UTF-16 code units (it's a
    /// non-BMP code point, U+1F642, encoded as a surrogate pair).
    /// `🙂a` is 2 chars / 3 UTF-16 units. The pair of helpers must
    /// round-trip exactly when the offset lands ON a char boundary.
    #[test]
    fn utf16_round_trip_for_emoji_text() {
        let text = "🙂a";
        // After the emoji: 1 char, 2 UTF-16 units.
        assert_eq!(utf16_offset_to_char(text, 2), 1);
        assert_eq!(char_offset_to_utf16(text, 1), 2);
        // End of string: 2 chars, 3 UTF-16 units.
        assert_eq!(utf16_offset_to_char(text, 3), 2);
        assert_eq!(char_offset_to_utf16(text, 2), 3);
        // Start of string: identity at 0.
        assert_eq!(utf16_offset_to_char(text, 0), 0);
        assert_eq!(char_offset_to_utf16(text, 0), 0);
    }

    /// UTF-16 offset *inside* a surrogate pair clamps to the char
    /// boundary BEFORE it — the browser's `Range` API will never put
    /// us inside a surrogate pair, but the helper must be defensive
    /// against bogus inputs (e.g. a buggy mutation observer).
    #[test]
    fn utf16_offset_inside_surrogate_clamps_to_previous_char() {
        let text = "🙂a";
        assert_eq!(utf16_offset_to_char(text, 1), 0);
    }

    /// Offsets past the end of `text` saturate at the scalar length
    /// (matches the browser's "selection at end of text node" case).
    #[test]
    fn utf16_offset_past_end_saturates() {
        let text = "ab";
        assert_eq!(utf16_offset_to_char(text, 99), 2);
        assert_eq!(char_offset_to_utf16(text, 99), 2);
    }

    /// BMP-only text round-trips trivially: char count == UTF-16 unit
    /// count, so neither helper changes the offset.
    #[test]
    fn ascii_text_is_identity() {
        let text = "hello, world";
        for offset in 0..=text.len() {
            assert_eq!(utf16_offset_to_char(text, offset as u32), offset);
            assert_eq!(char_offset_to_utf16(text, offset), offset as u32);
        }
    }
}
