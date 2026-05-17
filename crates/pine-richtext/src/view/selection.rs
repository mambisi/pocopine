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
pub fn dom_pos_to_model(surface: &Element, node: &DomNode, offset: u32) -> Option<usize> {
    if !node_inside_surface(surface, node) {
        return None;
    }

    // Text node: the caret is `offset` chars into this text node.
    if node.node_type() == DomNode::TEXT_NODE {
        let parent = node.parent_element()?;
        let textblock = nearest_tagged_ancestor(surface, &parent)?;
        let content_start = textblock_content_start(surface, &textblock);
        let inner = text_offset_within(&textblock, node)?;
        return Some(content_start + inner + offset as usize);
    }

    // Element node: `offset` is an index into its children. Sum the
    // model size of each child before that index.
    let element: Element = node.clone().dyn_into().ok()?;
    let content_start = textblock_content_start(surface, &element);
    let inner = sibling_size_before(&element, offset as usize);
    Some(content_start + inner)
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
/// (or, for the last child, from its visible text length plus
/// open+close tokens when it's a wrapper).
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
        if let Some(next_pos) = next_sibling_data_pos(child_el) {
            if let Some(our_pos) = data_pos_of(child_el) {
                acc += next_pos.saturating_sub(our_pos);
                continue;
            }
        }
        // Fall back: use textContent length + 2 (open/close tokens)
        // for wrappers, or 1 for leaves.
        let text_len = child_el.text_content().unwrap_or_default().chars().count();
        let is_leaf = child_el.children().length() == 0;
        acc += text_len + if is_leaf { 0 } else { 2 };
    }
    acc
}

fn next_sibling_data_pos(el: &Element) -> Option<usize> {
    let sibling = el.next_element_sibling()?;
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

    if target == content_start {
        return Some((dom_el.unchecked_into::<DomNode>(), 0));
    }

    let mut pos = content_start;
    let mut child_dom_index = 0u32;
    for child in model_node.content().iter() {
        let child_size = child.node_size();
        let child_end = pos + child_size;

        if target == pos {
            return Some((dom_el.unchecked_into::<DomNode>(), child_dom_index));
        }

        if target < child_end {
            if child.is_text() {
                let chars_in = target - pos;
                let text_node = dom_text_node_at(&dom_el, child_dom_index as usize)?;
                return Some((text_node, chars_in as u32));
            }
            if let Some(found) = walk_node(surface, child, Some(pos), target) {
                return Some(found);
            }
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
