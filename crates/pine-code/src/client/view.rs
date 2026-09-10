use std::collections::HashMap;

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Element, Node, Text};

use super::dom_reader::{self, DomPoint};
use crate::language::Token;
use crate::{CodeError, CodeResult, Selection, TextDocument, TextOffset, byte_to_utf16};

pub(super) fn dom_error(error: JsValue) -> CodeError {
    CodeError::ViewFailure(
        error
            .as_string()
            .unwrap_or_else(|| "DOM operation failed".into()),
    )
}

pub(super) struct LineView {
    pub element: Element,
    pub text: String,
    pub start: usize,
    pub tokens: Vec<Token>,
    id: String,
}

pub(super) struct View {
    pub root: Element,
    pub surface: Element,
    pub gutter: Element,
    pub status: Element,
    pub aria_owner: Element,
    pub lines: Vec<LineView>,
    index: HashMap<String, usize>,
    next_line: u64,
    gutter_lines: usize,
    pub changed_dom: bool,
    pub structure_changed: bool,
}

impl View {
    pub fn new(root: Element) -> CodeResult<Self> {
        let aria_owner = root
            .closest("pine-code-editor")
            .map_err(dom_error)?
            .unwrap_or_else(|| root.clone());
        let query = |selector| {
            root.query_selector(selector)
                .map_err(dom_error)?
                .ok_or(CodeError::ViewUnavailable)
        };
        Ok(Self {
            surface: query("[data-pine-code-content]")?,
            gutter: query("[data-pine-code-gutter]")?,
            status: query("[data-pine-code-status]")?,
            aria_owner,
            root,
            lines: Vec::new(),
            index: HashMap::new(),
            next_line: 0,
            gutter_lines: 0,
            changed_dom: false,
            structure_changed: false,
        })
    }

    pub fn focused(&self) -> bool {
        self.surface
            .owner_document()
            .and_then(|doc| doc.active_element())
            .is_some_and(|el| el.is_same_node(Some(&self.surface)))
    }

    pub fn recover_structure(&self) -> CodeResult<()> {
        for element in [&self.gutter, &self.surface, &self.status] {
            if !element
                .parent_element()
                .is_some_and(|parent| parent.is_same_node(Some(&self.root)))
            {
                self.root.append_child(element).map_err(dom_error)?;
            }
        }
        Ok(())
    }

    pub fn point_line(&self, node: &Node) -> Option<usize> {
        let mut element = node
            .dyn_ref::<Element>()
            .cloned()
            .or_else(|| node.parent_element());
        while let Some(current) = element {
            if current.is_same_node(Some(&self.surface)) {
                break;
            }
            if let Some(id) = current.get_attribute("data-pine-code-line") {
                return self.index.get(&id).copied();
            }
            element = current.parent_element();
        }
        None
    }

    pub fn selection(&self, document: &TextDocument) -> CodeResult<Option<Selection>> {
        let Some(points) = dom_reader::selection_points(&self.surface) else {
            return Ok(None);
        };
        if let Some(line) = self
            .point_line(&points[0].node)
            .filter(|line| Some(*line) == self.point_line(&points[1].node))
        {
            let view = &self.lines[line];
            let read = dom_reader::read(view.element.as_ref(), Some(&points))?;
            let selection = Selection::between(
                view.start + read.points[0].ok_or(CodeError::InvalidPosition)?,
                view.start + read.points[1].ok_or(CodeError::InvalidPosition)?,
            );
            document.check_selection(selection)?;
            return Ok(Some(selection));
        }
        let mut result = [0; 2];
        for (index, point) in points.iter().enumerate() {
            result[index] = self.offset(point)?;
        }
        let selection = Selection::between(result[0], result[1]);
        document.check_selection(selection)?;
        Ok(Some(selection))
    }

    pub fn offset(&self, point: &DomPoint) -> CodeResult<usize> {
        if point.node.is_same_node(Some(&self.surface)) {
            return self
                .lines
                .get(point.offset as usize)
                .map(|line| line.start)
                .or_else(|| {
                    (point.offset as usize == self.lines.len()).then(|| {
                        self.lines
                            .last()
                            .map_or(0, |line| line.start + line.text.len())
                    })
                })
                .ok_or(CodeError::InvalidPosition);
        }
        let index = self
            .point_line(&point.node)
            .ok_or(CodeError::InvalidPosition)?;
        let line = &self.lines[index];
        let points = [point.clone(), point.clone()];
        let read = dom_reader::read(line.element.as_ref(), Some(&points))?;
        Ok(line.start + read.points[0].ok_or(CodeError::InvalidPosition)?)
    }

    fn dom_point(&self, document: &TextDocument, offset: TextOffset) -> CodeResult<(Node, u32)> {
        let index = document.line_at(offset)?;
        let line = self.lines.get(index).ok_or(CodeError::ViewUnavailable)?;
        let mut remaining = offset.0 - line.start;
        fn descend(node: &Node, remaining: &mut usize) -> CodeResult<Option<(Node, u32)>> {
            if node.node_type() == Node::TEXT_NODE {
                let text = node.node_value().unwrap_or_default();
                if *remaining <= text.len() {
                    return Ok(Some((node.clone(), byte_to_utf16(&text, *remaining)?)));
                }
                *remaining -= text.len();
            } else {
                let children = node.child_nodes();
                for index in 0..children.length() {
                    if let Some(child) = children.item(index)
                        && let Some(point) = descend(&child, remaining)?
                    {
                        return Ok(Some(point));
                    }
                }
            }
            Ok(None)
        }
        descend(line.element.as_ref(), &mut remaining)?
            .or_else(|| (remaining == 0).then(|| (line.element.clone().into(), 0)))
            .ok_or(CodeError::InvalidPosition)
    }

    pub fn restore_selection(
        &self,
        document: &TextDocument,
        selection: Selection,
    ) -> CodeResult<()> {
        let (anchor, anchor_offset) = self.dom_point(document, selection.anchor)?;
        let (head, head_offset) = if selection.is_empty() {
            (anchor.clone(), anchor_offset)
        } else {
            self.dom_point(document, selection.head)?
        };
        let selection = self
            .surface
            .owner_document()
            .ok_or(CodeError::ViewUnavailable)?
            .get_selection()
            .map_err(dom_error)?
            .ok_or(CodeError::ViewUnavailable)?;
        selection
            .set_base_and_extent(&anchor, anchor_offset, &head, head_offset)
            .map_err(dom_error)
    }

    pub fn reconcile(&mut self, document: &TextDocument, repair: bool) -> CodeResult<()> {
        self.changed_dom = false;
        self.structure_changed = false;
        if !self.root.contains(Some(&self.surface)) {
            return Err(CodeError::ViewUnavailable);
        }
        let mut prefix = 0;
        while prefix < self.lines.len().min(document.line_count())
            && Some(self.lines[prefix].text.as_str()) == document.line(prefix)
        {
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < self.lines.len() - prefix
            && suffix < document.line_count() - prefix
            && Some(self.lines[self.lines.len() - 1 - suffix].text.as_str())
                == document.line(document.line_count() - 1 - suffix)
        {
            suffix += 1;
        }
        let old_middle = self.lines.len() - prefix - suffix;
        let new_middle = document.line_count() - prefix - suffix;
        let shared = old_middle.min(new_middle);
        for index in prefix..prefix + shared {
            if Self::patch_line(&self.lines[index].element, document.line(index).unwrap())? {
                self.lines[index].tokens.clear();
                self.changed_dom = true;
            }
            self.lines[index].text = document.line(index).unwrap().into();
        }
        for line in self.lines.drain(prefix + shared..prefix + old_middle) {
            self.changed_dom = true;
            if let Some(parent) = line.element.parent_node() {
                parent.remove_child(&line.element).map_err(dom_error)?;
            }
        }
        let doc = self
            .surface
            .owner_document()
            .ok_or(CodeError::ViewUnavailable)?;
        for index in prefix + shared..prefix + new_middle {
            self.changed_dom = true;
            let el = doc.create_element("div").map_err(dom_error)?;
            self.next_line += 1;
            let id = self.next_line.to_string();
            el.set_attribute("data-pine-code-line", &id)
                .map_err(dom_error)?;
            let text = document.line(index).unwrap();
            Self::patch_line(&el, text)?;
            let next = self
                .lines
                .get(index)
                .filter(|line| {
                    line.element
                        .parent_node()
                        .is_some_and(|p| p.is_same_node(Some(&self.surface)))
                })
                .map(|line| line.element.as_ref());
            self.surface.insert_before(&el, next).map_err(dom_error)?;
            self.lines.insert(
                index,
                LineView {
                    element: el,
                    text: text.into(),
                    start: 0,
                    tokens: Vec::new(),
                    id,
                },
            );
        }
        // Browser splits/joins may move retained wrappers. Restore their order
        // without replacing untouched nodes; discard unknown structural nodes.
        let structural = old_middle != new_middle;
        self.structure_changed = structural;
        if structural {
            self.index.clear();
        }
        for (index, line) in self.lines.iter_mut().enumerate() {
            line.start = document.line_start(index).unwrap();
            if structural {
                self.index.insert(line.id.clone(), index);
            }
            if repair {
                if Self::patch_line(&line.element, document.line(index).unwrap())? {
                    line.tokens.clear();
                    self.changed_dom = true;
                }
                let at = self.surface.child_nodes().item(index as u32);
                if !at
                    .as_ref()
                    .is_some_and(|at| at.is_same_node(Some(&line.element)))
                {
                    self.changed_dom = true;
                    self.surface
                        .insert_before(&line.element, at.as_ref())
                        .map_err(dom_error)?;
                }
            }
        }
        while self.surface.child_nodes().length() > self.lines.len() as u32 {
            self.changed_dom = true;
            self.surface
                .remove_child(&self.surface.last_child().unwrap())
                .map_err(dom_error)?;
        }
        if self.gutter_lines != document.line_count() {
            let numbers = (1..=document.line_count())
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            self.gutter.set_text_content(Some(&numbers));
            self.gutter_lines = document.line_count();
        }
        Ok(())
    }

    pub fn patch_line(element: &Element, text: &str) -> CodeResult<bool> {
        let doc = element.owner_document().ok_or(CodeError::ViewUnavailable)?;
        if text.is_empty() {
            let canonical = element.child_nodes().length() == 1
                && element
                    .first_element_child()
                    .is_some_and(|el| el.has_attribute("data-pine-code-empty"));
            if !canonical {
                element.set_text_content(None);
                let br = doc.create_element("br").map_err(dom_error)?;
                br.set_attribute("data-pine-code-empty", "")
                    .map_err(dom_error)?;
                element.append_child(&br).map_err(dom_error)?;
            }
            return Ok(!canonical);
        }
        // Keep our transparent syntax spans when native input already produced
        // exactly the new text. Foreign block/formatting elements are flattened.
        let children = element.child_nodes();
        if element.text_content().as_deref() == Some(text)
            && (0..children.length()).all(|index| {
                children.item(index).is_some_and(|node| {
                    node.node_type() == Node::TEXT_NODE
                        || node.dyn_ref::<Element>().is_some_and(|el| {
                            el.local_name() == "span"
                                && el.has_attribute("data-token")
                                && el.child_nodes().length() == 1
                                && el
                                    .first_child()
                                    .is_some_and(|child| child.node_type() == Node::TEXT_NODE)
                        })
                })
            })
        {
            return Ok(false);
        }
        if element.child_nodes().length() == 1
            && let Some(node) = element
                .first_child()
                .and_then(|node| node.dyn_into::<Text>().ok())
        {
            let old = node.data();
            if old != text {
                let prefix = old
                    .chars()
                    .zip(text.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.len_utf8())
                    .sum::<usize>();
                let suffix = old[prefix..]
                    .chars()
                    .rev()
                    .zip(text[prefix..].chars().rev())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.len_utf8())
                    .sum::<usize>();
                node.replace_data(
                    byte_to_utf16(&old, prefix)?,
                    byte_to_utf16(
                        &old[prefix..old.len() - suffix],
                        old.len() - suffix - prefix,
                    )?,
                    &text[prefix..text.len() - suffix],
                )
                .map_err(dom_error)?;
            }
            return Ok(true);
        }
        element.set_text_content(None);
        element
            .append_child(&doc.create_text_node(text))
            .map_err(dom_error)?;
        Ok(true)
    }

    pub fn patch_tokens(&mut self, index: usize, tokens: &[Token]) -> CodeResult<bool> {
        let line = self
            .lines
            .get_mut(index)
            .ok_or(CodeError::ViewUnavailable)?;
        crate::language::validate_tokens(&line.text, tokens)
            .map_err(|_| CodeError::ViewFailure("invalid highlight ranges".into()))?;
        let document = line
            .element
            .owner_document()
            .ok_or(CodeError::ViewUnavailable)?;
        let mut parts = Vec::new();
        let mut end = 0;
        for token in tokens {
            if end < token.range.from.0 {
                parts.push((None, &line.text[end..token.range.from.0]));
            }
            parts.push((
                Some(token.kind),
                &line.text[token.range.from.0..token.range.to.0],
            ));
            end = token.range.to.0;
        }
        if end < line.text.len() {
            parts.push((None, &line.text[end..]));
        }
        // Native editing can extend a syntax span without changing the
        // tokenizer's ranges (typing " fu" after the keyword "let"). Cached
        // ranges alone do not prove that the browser DOM still matches them.
        if line.tokens == tokens && !parts.is_empty() {
            let children = line.element.child_nodes();
            let matches = children.length() as usize == parts.len()
                && parts.iter().enumerate().all(|(index, (kind, text))| {
                    children.item(index as u32).is_some_and(|node| {
                        let shape_matches = match kind {
                            None => node.node_type() == Node::TEXT_NODE,
                            Some(kind) => node.dyn_ref::<Element>().is_some_and(|el| {
                                el.local_name() == "span"
                                    && el.get_attribute("data-token").as_deref()
                                        == Some(kind.class())
                                    && el.child_nodes().length() == 1
                                    && el
                                        .first_child()
                                        .is_some_and(|child| child.node_type() == Node::TEXT_NODE)
                            }),
                        };
                        shape_matches && node.text_content().as_deref() == Some(text)
                    })
                });
            if matches {
                return Ok(false);
            }
        }
        if parts.is_empty() {
            let changed = Self::patch_line(&line.element, "")?;
            line.tokens.clear();
            return Ok(changed);
        } else {
            for (index, (kind, text)) in parts.iter().enumerate() {
                let existing = line.element.child_nodes().item(index as u32);
                let reusable = existing.as_ref().is_some_and(|node| match kind {
                    None => node.node_type() == Node::TEXT_NODE,
                    Some(kind) => node.dyn_ref::<Element>().is_some_and(|el| {
                        el.local_name() == "span"
                            && el.get_attribute("data-token").as_deref() == Some(kind.class())
                    }),
                });
                if reusable {
                    let node = existing.unwrap();
                    if node.text_content().as_deref() != Some(text) {
                        node.set_text_content(Some(text));
                    }
                } else {
                    let node: Node = if let Some(kind) = kind {
                        let span = document.create_element("span").map_err(dom_error)?;
                        span.set_attribute("data-token", kind.class())
                            .map_err(dom_error)?;
                        span.append_child(&document.create_text_node(text))
                            .map_err(dom_error)?;
                        span.into()
                    } else {
                        document.create_text_node(text).into()
                    };
                    if let Some(existing) = existing {
                        line.element
                            .replace_child(&node, &existing)
                            .map_err(dom_error)?;
                    } else {
                        line.element.append_child(&node).map_err(dom_error)?;
                    }
                }
            }
            while line.element.child_nodes().length() > parts.len() as u32 {
                line.element
                    .remove_child(&line.element.last_child().unwrap())
                    .map_err(dom_error)?;
            }
        }
        line.tokens = tokens.to_vec();
        Ok(true)
    }
}
