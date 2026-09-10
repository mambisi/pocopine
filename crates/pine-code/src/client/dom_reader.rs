//! Plain text and selection are read together, with explicit block boundaries.

use wasm_bindgen::JsCast;
use web_sys::{Element, Node};

use crate::{CodeResult, normalize_lf, utf16_to_byte};

#[derive(Clone)]
pub(super) struct DomPoint {
    pub node: Node,
    pub offset: u32,
}

#[derive(Default)]
pub(super) struct DomRead {
    pub text: String,
    pub points: [Option<usize>; 2],
}

pub(super) fn selection_points(surface: &Element) -> Option<[DomPoint; 2]> {
    let selection = surface.owner_document()?.get_selection().ok()??;
    let anchor = selection.anchor_node()?;
    let head = selection.focus_node()?;
    if !surface.contains(Some(&anchor)) || !surface.contains(Some(&head)) {
        return None;
    }
    Some([
        DomPoint {
            node: anchor,
            offset: selection.anchor_offset(),
        },
        DomPoint {
            node: head,
            offset: selection.focus_offset(),
        },
    ])
}

fn is_block(node: &Node) -> bool {
    node.dyn_ref::<Element>().is_some_and(|el| {
        matches!(
            el.local_name().as_str(),
            "div" | "p" | "li" | "pre" | "section" | "blockquote" | "h1" | "h2" | "h3"
        )
    })
}

pub(super) fn read(node: &Node, points: Option<&[DomPoint; 2]>) -> CodeResult<DomRead> {
    let mut result = DomRead::default();
    if node.node_type() == Node::TEXT_NODE {
        let text = node.node_value().unwrap_or_default();
        if let Some(points) = points {
            for (index, point) in points.iter().enumerate() {
                if point.node.is_same_node(Some(node)) {
                    let byte = utf16_to_byte(&text, point.offset)?;
                    result.points[index] = Some(normalize_lf(&text[..byte]).len());
                }
            }
        }
        result.text = normalize_lf(&text);
        return Ok(result);
    }
    if node.node_type() != Node::ELEMENT_NODE && node.node_type() != Node::DOCUMENT_FRAGMENT_NODE {
        return Ok(result);
    }
    let children = node.child_nodes();
    let mut previous_block = false;
    let mut has_piece = false;
    for index in 0..=children.length() {
        let child = children.item(index);
        let block = child.as_ref().is_some_and(is_block);
        let fragment = child
            .as_ref()
            .map(|child| read(child, points))
            .transpose()?;
        let meaningful = block || fragment.as_ref().is_some_and(|part| !part.text.is_empty());
        if meaningful && has_piece && (previous_block || block) {
            result.text.push('\n');
        }
        if let Some(points) = points {
            for (slot, point) in points.iter().enumerate() {
                if point.node.is_same_node(Some(node)) && point.offset == index {
                    result.points[slot] = Some(result.text.len());
                }
            }
        }
        let Some(child) = child else {
            break;
        };
        if let Some(el) = child.dyn_ref::<Element>()
            && el.local_name() == "br"
        {
            // A final BR is the browser's empty-line placeholder. A
            // preceding BR contributes the intentional newline.
            if !el.has_attribute("data-pine-code-empty") && index + 1 < children.length() {
                result.text.push('\n');
                has_piece = true;
                previous_block = false;
            }
            continue;
        }
        if let Some(fragment) = fragment {
            for slot in 0..2 {
                if let Some(offset) = fragment.points[slot] {
                    result.points[slot] = Some(result.text.len() + offset);
                }
            }
            result.text.push_str(&fragment.text);
        }
        if meaningful {
            has_piece = true;
            previous_block = block;
        }
    }
    Ok(result)
}
