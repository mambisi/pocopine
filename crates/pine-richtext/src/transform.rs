use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Attrs, Fragment, Mark, Node, NodeRange, NodeType, ResolvedPos, Schema, Slice};
use crate::{RichTextError, RichTextResult};

/// Result of applying a step.
#[derive(Clone, Debug, PartialEq)]
pub struct StepResult {
    /// Updated document.
    pub doc: Node,
    /// Position map produced by the step.
    pub map: StepMap,
}

/// A range in the current document that covers replaced content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangedRange {
    /// Inclusive start position in the transformed document.
    pub from: usize,
    /// Exclusive end position in the transformed document.
    pub to: usize,
}

/// A single transform step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    /// Replace content between two positions.
    Replace(ReplaceStep),
    /// Replace a range while preserving a gap inside the replacement.
    ReplaceAround(ReplaceAroundStep),
    /// Add a mark across an inline range.
    AddMark(MarkStep),
    /// Remove a mark across an inline range.
    RemoveMark(MarkStep),
    /// Add a mark to the node at a position.
    AddNodeMark(NodeMarkStep),
    /// Remove a mark from the node at a position.
    RemoveNodeMark(NodeMarkStep),
    /// Set or remove an attribute on the node at a position.
    Attr(AttrStep),
    /// Set or remove an attribute on the document node.
    DocAttr(DocAttrStep),
}

impl Step {
    /// Apply the step to a document.
    pub fn apply(&self, doc: &Node, schema: &Schema) -> RichTextResult<StepResult> {
        match self {
            Self::Replace(step) => {
                let new_doc = replace(doc, step.from, step.to, step.slice.clone(), schema)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: StepMap::single(step.from, step.to - step.from, step.slice.size()),
                })
            }
            Self::ReplaceAround(step) => {
                let new_doc = replace_around(doc, step, schema)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: step.step_map(),
                })
            }
            Self::AddMark(step) => {
                let new_doc = apply_mark(doc, step.from, step.to, &step.mark, true, schema)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: StepMap::empty(),
                })
            }
            Self::RemoveMark(step) => {
                let new_doc = apply_mark(doc, step.from, step.to, &step.mark, false, schema)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: StepMap::empty(),
                })
            }
            Self::AddNodeMark(step) => {
                let new_doc = apply_node_mark(doc, step.pos, &step.mark, true, schema)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: StepMap::empty(),
                })
            }
            Self::RemoveNodeMark(step) => {
                let new_doc = apply_node_mark(doc, step.pos, &step.mark, false, schema)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: StepMap::empty(),
                })
            }
            Self::Attr(step) => {
                let new_doc =
                    apply_node_attr(doc, step.pos, &step.attr, step.value.clone(), schema)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: StepMap::empty(),
                })
            }
            Self::DocAttr(step) => {
                let mut new_doc = doc.clone();
                if let Some(value) = step.value.clone() {
                    new_doc.attrs.insert(step.attr.clone(), value);
                } else {
                    new_doc.attrs.remove(&step.attr);
                }
                schema.check_node(&new_doc)?;
                Ok(StepResult {
                    doc: new_doc,
                    map: StepMap::empty(),
                })
            }
        }
    }

    /// Invert a step relative to the document it was applied to.
    pub fn invert(&self, old_doc: &Node, schema: &Schema) -> RichTextResult<Step> {
        match self {
            Self::Replace(step) => {
                let old_slice = slice_for_replace(old_doc, step.from, step.to)?;
                Ok(Self::Replace(ReplaceStep {
                    from: step.from,
                    to: step.from + step.slice.size(),
                    slice: old_slice,
                    structure: step.structure,
                }))
            }
            Self::ReplaceAround(step) => {
                let gap = step.gap_to - step.gap_from;
                Ok(Self::ReplaceAround(ReplaceAroundStep {
                    from: step.from,
                    to: step.from + step.slice.size() + gap,
                    gap_from: step.from + step.insert,
                    gap_to: step.from + step.insert + gap,
                    slice: old_doc
                        .slice(step.from, step.to)?
                        .remove_between(step.gap_from - step.from, step.gap_to - step.from)?,
                    insert: step.gap_from - step.from,
                    structure: step.structure,
                }))
            }
            Self::AddMark(step) => Ok(Self::RemoveMark(step.clone())),
            Self::RemoveMark(step) => Ok(Self::AddMark(step.clone())),
            Self::AddNodeMark(step) => Ok(Self::RemoveNodeMark(step.clone())),
            Self::RemoveNodeMark(step) => Ok(Self::AddNodeMark(step.clone())),
            Self::Attr(step) => {
                let node =
                    node_at(old_doc, step.pos).ok_or_else(|| RichTextError::InvalidPosition {
                        pos: step.pos,
                        size: old_doc.content_size(),
                    })?;
                Ok(Self::Attr(AttrStep {
                    pos: step.pos,
                    attr: step.attr.clone(),
                    value: node.attrs.get(&step.attr).cloned(),
                }))
            }
            Self::DocAttr(step) => Ok(Self::DocAttr(DocAttrStep {
                attr: step.attr.clone(),
                value: old_doc.attrs.get(&step.attr).cloned(),
            })),
        }
        .and_then(|step| {
            if matches!(step, Self::Replace(_)) {
                schema.check_node(old_doc)?;
            }
            Ok(step)
        })
    }

    /// Position map for this step without applying it.
    pub fn map(&self) -> StepMap {
        match self {
            Self::Replace(step) => {
                StepMap::single(step.from, step.to - step.from, step.slice.size())
            }
            Self::ReplaceAround(step) => step.step_map(),
            _ => StepMap::empty(),
        }
    }

    /// Map this step through a mapping, dropping it when its range was deleted.
    pub fn map_step(&self, mapping: &Mapping) -> Option<Step> {
        match self {
            Self::Replace(step) => {
                let to = mapping.map_result(step.to, -1);
                let from = mapping.map_result(step.from, 1);
                if from.deleted_across && to.deleted_across {
                    None
                } else {
                    Some(Self::Replace(ReplaceStep {
                        from: from.pos,
                        to: from.pos.max(to.pos),
                        slice: step.slice.clone(),
                        structure: step.structure,
                    }))
                }
            }
            Self::ReplaceAround(step) => {
                let from = mapping.map_result(step.from, 1);
                let to = mapping.map_result(step.to, -1);
                let gap_from = if step.from == step.gap_from {
                    from.pos
                } else {
                    mapping.map(step.gap_from, -1)
                };
                let gap_to = if step.to == step.gap_to {
                    to.pos
                } else {
                    mapping.map(step.gap_to, 1)
                };
                if (from.deleted_across && to.deleted_across)
                    || gap_from < from.pos
                    || gap_to > to.pos
                {
                    None
                } else {
                    Some(Self::ReplaceAround(ReplaceAroundStep {
                        from: from.pos,
                        to: to.pos,
                        gap_from,
                        gap_to,
                        slice: step.slice.clone(),
                        insert: step.insert,
                        structure: step.structure,
                    }))
                }
            }
            Self::AddMark(step) => map_mark_step(step, mapping).map(Self::AddMark),
            Self::RemoveMark(step) => map_mark_step(step, mapping).map(Self::RemoveMark),
            Self::AddNodeMark(step) => map_node_step(step, mapping).map(Self::AddNodeMark),
            Self::RemoveNodeMark(step) => map_node_step(step, mapping).map(Self::RemoveNodeMark),
            Self::Attr(step) => {
                let pos = mapping.map_result(step.pos, 1);
                (!pos.deleted_after).then(|| {
                    Self::Attr(AttrStep {
                        pos: pos.pos,
                        attr: step.attr.clone(),
                        value: step.value.clone(),
                    })
                })
            }
            Self::DocAttr(step) => Some(Self::DocAttr(step.clone())),
        }
    }

    /// Merge this step with a following step when they form one contiguous edit.
    pub fn merge(&self, other: &Step) -> Option<Step> {
        match (self, other) {
            (Self::Replace(left), Self::Replace(right)) => left.merge(right).map(Self::Replace),
            (Self::AddMark(left), Self::AddMark(right)) if left.mark == right.mark => {
                left.merge_range(right).map(Self::AddMark)
            }
            (Self::RemoveMark(left), Self::RemoveMark(right)) if left.mark == right.mark => {
                left.merge_range(right).map(Self::RemoveMark)
            }
            _ => None,
        }
    }

    /// Serialize this step to JSON using upstream ProseMirror's shape.
    /// The result always carries a `stepType` discriminator plus the step's
    /// fields; `Slice`, `Mark`, and `Node` fields are emitted in their PM
    /// JSON forms.
    pub fn to_json(&self) -> Value {
        use serde_json::json;
        match self {
            Self::Replace(step) => {
                let mut obj = serde_json::Map::new();
                obj.insert("stepType".to_string(), json!("replace"));
                obj.insert("from".to_string(), json!(step.from));
                obj.insert("to".to_string(), json!(step.to));
                if !step.slice.content.is_empty()
                    || step.slice.open_start != 0
                    || step.slice.open_end != 0
                {
                    obj.insert(
                        "slice".to_string(),
                        serde_json::to_value(&step.slice).unwrap_or(Value::Null),
                    );
                }
                Value::Object(obj)
            }
            Self::ReplaceAround(step) => {
                let mut obj = serde_json::Map::new();
                obj.insert("stepType".to_string(), json!("replaceAround"));
                obj.insert("from".to_string(), json!(step.from));
                obj.insert("to".to_string(), json!(step.to));
                obj.insert("gapFrom".to_string(), json!(step.gap_from));
                obj.insert("gapTo".to_string(), json!(step.gap_to));
                obj.insert("insert".to_string(), json!(step.insert));
                obj.insert(
                    "slice".to_string(),
                    serde_json::to_value(&step.slice).unwrap_or(Value::Null),
                );
                if step.structure {
                    obj.insert("structure".to_string(), json!(true));
                }
                Value::Object(obj)
            }
            Self::AddMark(step) => mark_step_to_json("addMark", step),
            Self::RemoveMark(step) => mark_step_to_json("removeMark", step),
            Self::AddNodeMark(step) => node_mark_step_to_json("addNodeMark", step),
            Self::RemoveNodeMark(step) => node_mark_step_to_json("removeNodeMark", step),
            Self::Attr(step) => {
                let mut obj = serde_json::Map::new();
                obj.insert("stepType".to_string(), json!("attr"));
                obj.insert("pos".to_string(), json!(step.pos));
                obj.insert("attr".to_string(), json!(step.attr));
                obj.insert(
                    "value".to_string(),
                    step.value.clone().unwrap_or(Value::Null),
                );
                Value::Object(obj)
            }
            Self::DocAttr(step) => {
                let mut obj = serde_json::Map::new();
                obj.insert("stepType".to_string(), json!("docAttr"));
                obj.insert("attr".to_string(), json!(step.attr));
                obj.insert(
                    "value".to_string(),
                    step.value.clone().unwrap_or(Value::Null),
                );
                Value::Object(obj)
            }
        }
    }

    /// Reconstruct a step from its JSON representation. The schema is used
    /// to validate referenced node and mark types.
    pub fn from_json(schema: &Schema, value: Value) -> RichTextResult<Step> {
        let obj = match value {
            Value::Object(o) => o,
            _ => {
                return Err(RichTextError::Step(
                    "step JSON must be an object".to_string(),
                ));
            }
        };
        let step_type = obj
            .get("stepType")
            .and_then(Value::as_str)
            .ok_or_else(|| RichTextError::Step("missing stepType".to_string()))?
            .to_string();
        match step_type.as_str() {
            "replace" => {
                let from = json_usize(&obj, "from")?;
                let to = json_usize(&obj, "to")?;
                let slice = obj
                    .get("slice")
                    .map(|v| slice_from_json(schema, v))
                    .transpose()?
                    .unwrap_or_else(Slice::empty);
                Ok(Step::Replace(ReplaceStep::new(from, to, slice)))
            }
            "replaceAround" => {
                let from = json_usize(&obj, "from")?;
                let to = json_usize(&obj, "to")?;
                let gap_from = json_usize(&obj, "gapFrom")?;
                let gap_to = json_usize(&obj, "gapTo")?;
                let insert = json_usize(&obj, "insert")?;
                let slice = slice_from_json(
                    schema,
                    obj.get("slice")
                        .ok_or_else(|| RichTextError::Step("missing slice".to_string()))?,
                )?;
                let structure = obj
                    .get("structure")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(Step::ReplaceAround(ReplaceAroundStep {
                    from,
                    to,
                    gap_from,
                    gap_to,
                    slice,
                    insert,
                    structure,
                }))
            }
            "addMark" | "removeMark" => {
                let from = json_usize(&obj, "from")?;
                let to = json_usize(&obj, "to")?;
                let mark = mark_from_json(
                    schema,
                    obj.get("mark")
                        .ok_or_else(|| RichTextError::Step("missing mark".to_string()))?,
                )?;
                let step = MarkStep::new(from, to, mark);
                Ok(if step_type == "addMark" {
                    Step::AddMark(step)
                } else {
                    Step::RemoveMark(step)
                })
            }
            "addNodeMark" | "removeNodeMark" => {
                let pos = json_usize(&obj, "pos")?;
                let mark = mark_from_json(
                    schema,
                    obj.get("mark")
                        .ok_or_else(|| RichTextError::Step("missing mark".to_string()))?,
                )?;
                let step = NodeMarkStep { pos, mark };
                Ok(if step_type == "addNodeMark" {
                    Step::AddNodeMark(step)
                } else {
                    Step::RemoveNodeMark(step)
                })
            }
            "attr" => {
                let pos = json_usize(&obj, "pos")?;
                let attr = obj
                    .get("attr")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RichTextError::Step("missing attr".to_string()))?
                    .to_string();
                let value = match obj.get("value") {
                    Some(Value::Null) | None => None,
                    Some(other) => Some(other.clone()),
                };
                Ok(Step::Attr(AttrStep { pos, attr, value }))
            }
            "docAttr" => {
                let attr = obj
                    .get("attr")
                    .and_then(Value::as_str)
                    .ok_or_else(|| RichTextError::Step("missing attr".to_string()))?
                    .to_string();
                let value = match obj.get("value") {
                    Some(Value::Null) | None => None,
                    Some(other) => Some(other.clone()),
                };
                Ok(Step::DocAttr(DocAttrStep { attr, value }))
            }
            other => Err(RichTextError::Step(format!("unknown step type {other}"))),
        }
    }
}

fn mark_step_to_json(name: &str, step: &MarkStep) -> Value {
    use serde_json::json;
    let mut obj = serde_json::Map::new();
    obj.insert("stepType".to_string(), json!(name));
    obj.insert("from".to_string(), json!(step.from));
    obj.insert("to".to_string(), json!(step.to));
    obj.insert(
        "mark".to_string(),
        serde_json::to_value(&step.mark).unwrap_or(Value::Null),
    );
    Value::Object(obj)
}

fn node_mark_step_to_json(name: &str, step: &NodeMarkStep) -> Value {
    use serde_json::json;
    let mut obj = serde_json::Map::new();
    obj.insert("stepType".to_string(), json!(name));
    obj.insert("pos".to_string(), json!(step.pos));
    obj.insert(
        "mark".to_string(),
        serde_json::to_value(&step.mark).unwrap_or(Value::Null),
    );
    Value::Object(obj)
}

fn slice_from_json(schema: &Schema, value: &Value) -> RichTextResult<Slice> {
    let obj = value
        .as_object()
        .ok_or_else(|| RichTextError::Step("slice JSON must be an object".to_string()))?;
    let content = match obj.get("content") {
        Some(Value::Array(children)) => {
            let mut nodes = Vec::with_capacity(children.len());
            for child in children {
                nodes.push(node_from_json(schema, child)?);
            }
            Fragment::from(nodes)
        }
        Some(Value::Null) | None => Fragment::empty(),
        Some(_) => {
            return Err(RichTextError::Step(
                "slice content must be an array".to_string(),
            ));
        }
    };
    let open_start = obj.get("openStart").and_then(Value::as_u64).unwrap_or(0) as usize;
    let open_end = obj.get("openEnd").and_then(Value::as_u64).unwrap_or(0) as usize;
    Ok(Slice::new(content, open_start, open_end))
}

fn mark_from_json(schema: &Schema, value: &Value) -> RichTextResult<Mark> {
    let json = value.clone();
    let mark: Mark = serde_json::from_value(json)
        .map_err(|e| RichTextError::Step(format!("invalid mark JSON: {e}")))?;
    // Re-build through the schema so attribute defaults / required-attr
    // checks fire (matches PM's Schema.markFromJSON validation).
    schema.mark(mark.type_name(), mark.attrs().clone())
}

fn node_from_json(_schema: &Schema, value: &Value) -> RichTextResult<Node> {
    serde_json::from_value(value.clone())
        .map_err(|e| RichTextError::Step(format!("invalid node JSON: {e}")))
}

fn json_usize(obj: &serde_json::Map<String, Value>, key: &str) -> RichTextResult<usize> {
    obj.get(key)
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .ok_or_else(|| RichTextError::Step(format!("missing or invalid {key}")))
}

fn map_mark_step(step: &MarkStep, mapping: &Mapping) -> Option<MarkStep> {
    let from = mapping.map_result(step.from, 1);
    let to = mapping.map_result(step.to, -1);
    (!((from.deleted && to.deleted) || from.pos >= to.pos)).then(|| MarkStep {
        from: from.pos,
        to: to.pos,
        mark: step.mark.clone(),
    })
}

fn map_node_step(step: &NodeMarkStep, mapping: &Mapping) -> Option<NodeMarkStep> {
    let pos = mapping.map_result(step.pos, 1);
    (!pos.deleted_after).then(|| NodeMarkStep {
        pos: pos.pos,
        mark: step.mark.clone(),
    })
}

fn merge_slices(left: &Slice, right: &Slice) -> Slice {
    if left.size() + right.size() == 0 {
        Slice::empty()
    } else {
        Slice::new(
            left.content.clone().append(right.content.clone()),
            left.open_start,
            right.open_end,
        )
    }
}

/// Replace step data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplaceStep {
    /// Start position.
    pub from: usize,
    /// End position.
    pub to: usize,
    /// Replacement slice.
    pub slice: Slice,
    /// Whether the step should fail across structural boundaries.
    pub structure: bool,
}

impl ReplaceStep {
    /// Construct a replace step.
    pub fn new(from: usize, to: usize, slice: Slice) -> Self {
        Self {
            from,
            to,
            slice,
            structure: false,
        }
    }

    fn merge(&self, other: &Self) -> Option<Self> {
        if self.structure || other.structure {
            return None;
        }

        if self.from + self.slice.size() == other.from
            && self.slice.open_end == 0
            && other.slice.open_start == 0
        {
            let slice = merge_slices(&self.slice, &other.slice);
            Some(Self {
                from: self.from,
                to: self.to + (other.to - other.from),
                slice,
                structure: self.structure,
            })
        } else if other.to == self.from && self.slice.open_start == 0 && other.slice.open_end == 0 {
            let slice = merge_slices(&other.slice, &self.slice);
            Some(Self {
                from: other.from,
                to: self.to,
                slice,
                structure: self.structure,
            })
        } else {
            None
        }
    }
}

/// Replace-around step data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplaceAroundStep {
    /// Start position.
    pub from: usize,
    /// End position.
    pub to: usize,
    /// Start of preserved gap.
    pub gap_from: usize,
    /// End of preserved gap.
    pub gap_to: usize,
    /// Replacement slice.
    pub slice: Slice,
    /// Position inside `slice` where the gap is inserted.
    pub insert: usize,
    /// Whether the step should fail across structural boundaries.
    pub structure: bool,
}

impl ReplaceAroundStep {
    /// Construct a replace-around step.
    pub fn new(
        from: usize,
        to: usize,
        gap_from: usize,
        gap_to: usize,
        slice: Slice,
        insert: usize,
    ) -> Self {
        Self {
            from,
            to,
            gap_from,
            gap_to,
            slice,
            insert,
            structure: false,
        }
    }

    fn step_map(&self) -> StepMap {
        StepMap::new(vec![
            MapRange {
                old_start: self.from,
                old_size: self.gap_from - self.from,
                new_size: self.insert,
            },
            MapRange {
                old_start: self.gap_to,
                old_size: self.to - self.gap_to,
                new_size: self.slice.size() - self.insert,
            },
        ])
    }
}

/// Add/remove mark step data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkStep {
    /// Start position.
    pub from: usize,
    /// End position.
    pub to: usize,
    /// Mark to add or remove.
    pub mark: Mark,
}

impl MarkStep {
    /// Construct a mark step.
    pub fn new(from: usize, to: usize, mark: Mark) -> Self {
        Self { from, to, mark }
    }

    fn merge_range(&self, other: &Self) -> Option<Self> {
        (self.from <= other.to && self.to >= other.from).then(|| Self {
            from: self.from.min(other.from),
            to: self.to.max(other.to),
            mark: self.mark.clone(),
        })
    }
}

/// Add/remove node mark step data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeMarkStep {
    /// Position before the target node.
    pub pos: usize,
    /// Mark to add or remove.
    pub mark: Mark,
}

/// Node attribute step data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttrStep {
    /// Position before the target node.
    pub pos: usize,
    /// Attribute key.
    pub attr: String,
    /// New value, or `None` to remove the attribute.
    pub value: Option<Value>,
}

/// Document attribute step data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocAttrStep {
    /// Attribute key.
    pub attr: String,
    /// New value, or `None` to remove the attribute.
    pub value: Option<Value>,
}

/// One range in a [`StepMap`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapRange {
    /// Start position in the old document.
    pub old_start: usize,
    /// Size replaced in the old document.
    pub old_size: usize,
    /// Size inserted in the new document.
    pub new_size: usize,
}

/// Position mapping result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapResult {
    /// Mapped position.
    pub pos: usize,
    /// Whether the original position was inside deleted content.
    pub deleted: bool,
    /// Whether content before the original position was deleted.
    pub deleted_before: bool,
    /// Whether content after the original position was deleted.
    pub deleted_after: bool,
    /// Whether deleted content crossed over the original position.
    pub deleted_across: bool,
    recover: Option<MapRecover>,
}

impl MapResult {
    /// Opaque recovery token for lossless mirrored mappings, when available.
    pub fn recover(&self) -> Option<MapRecover> {
        self.recover
    }
}

/// Opaque recovery token produced when a position falls inside a changed range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapRecover {
    index: usize,
    offset: usize,
}

/// A single-step position map.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepMap {
    /// Replaced ranges.
    pub ranges: Vec<MapRange>,
    /// Whether this map is being read in reverse.
    #[serde(default)]
    pub inverted: bool,
}

impl StepMap {
    /// Empty map.
    pub fn empty() -> Self {
        Self {
            ranges: Vec::new(),
            inverted: false,
        }
    }

    /// Construct a map from explicit changed ranges.
    pub fn new(ranges: Vec<MapRange>) -> Self {
        Self {
            ranges,
            inverted: false,
        }
    }

    /// Map with one replaced range.
    pub fn single(old_start: usize, old_size: usize, new_size: usize) -> Self {
        if old_size == 0 && new_size == 0 {
            Self::empty()
        } else {
            Self {
                ranges: vec![MapRange {
                    old_start,
                    old_size,
                    new_size,
                }],
                inverted: false,
            }
        }
    }

    /// Map that shifts all positions by an offset.
    pub fn offset(offset: isize) -> Self {
        match offset.cmp(&0) {
            std::cmp::Ordering::Equal => Self::empty(),
            std::cmp::Ordering::Less => Self::single(0, offset.unsigned_abs(), 0),
            std::cmp::Ordering::Greater => Self::single(0, 0, offset as usize),
        }
    }

    /// Map a position.
    pub fn map(&self, pos: usize, assoc: i8) -> usize {
        self.map_result(pos, assoc).pos
    }

    /// Map a position and return deletion metadata.
    pub fn map_result(&self, pos: usize, assoc: i8) -> MapResult {
        let mut diff: isize = 0;
        for (index, range) in self.ranges.iter().enumerate() {
            let start = if self.inverted {
                add_diff(range.old_start, -diff)
            } else {
                range.old_start
            };
            let old_size = if self.inverted {
                range.new_size
            } else {
                range.old_size
            };
            let new_size = if self.inverted {
                range.old_size
            } else {
                range.new_size
            };
            let end = start + old_size;
            if start > pos {
                break;
            }
            if pos <= end {
                let side = if old_size == 0 {
                    assoc
                } else if pos == start {
                    -1
                } else if pos == end {
                    1
                } else {
                    assoc
                };
                let recover = if pos == if assoc < 0 { start } else { end } {
                    None
                } else {
                    Some(MapRecover {
                        index,
                        offset: pos - start,
                    })
                };
                let inside = pos > start && pos < end;
                return MapResult {
                    pos: add_diff(start + if side < 0 { 0 } else { new_size }, diff),
                    deleted: if assoc < 0 { pos != start } else { pos != end },
                    deleted_before: if pos == start {
                        false
                    } else {
                        pos == end || inside
                    },
                    deleted_after: pos == start || inside,
                    deleted_across: inside,
                    recover,
                };
            }
            diff += new_size as isize - old_size as isize;
        }

        MapResult {
            pos: add_diff(pos, diff),
            deleted: false,
            deleted_before: false,
            deleted_after: false,
            deleted_across: false,
            recover: None,
        }
    }

    /// Invert this map.
    pub fn invert(&self) -> Self {
        Self {
            ranges: self.ranges.clone(),
            inverted: !self.inverted,
        }
    }

    /// Call a function for each changed range.
    pub fn for_each<F>(&self, mut f: F)
    where
        F: FnMut(usize, usize, usize, usize),
    {
        let mut diff: isize = 0;
        for range in &self.ranges {
            let old_start = if self.inverted {
                add_diff(range.old_start, -diff)
            } else {
                range.old_start
            };
            let new_start = if self.inverted {
                range.old_start
            } else {
                add_diff(range.old_start, diff)
            };
            let old_size = if self.inverted {
                range.new_size
            } else {
                range.old_size
            };
            let new_size = if self.inverted {
                range.old_size
            } else {
                range.new_size
            };
            f(
                old_start,
                old_start + old_size,
                new_start,
                new_start + new_size,
            );
            diff += new_size as isize - old_size as isize;
        }
    }

    /// Recover the original position represented by a recovery token.
    pub fn recover(&self, recover: MapRecover) -> Option<usize> {
        let range = self.ranges.get(recover.index)?;
        let mut diff: isize = 0;
        if !self.inverted {
            for before in &self.ranges[..recover.index] {
                diff += before.new_size as isize - before.old_size as isize;
            }
        }
        Some(add_diff(range.old_start + recover.offset, diff))
    }

    /// Whether a position touches the changed range that created a recovery token.
    pub fn touches(&self, pos: usize, recover: MapRecover) -> bool {
        let mut diff: isize = 0;
        for (index, range) in self.ranges.iter().enumerate() {
            let start = if self.inverted {
                add_diff(range.old_start, -diff)
            } else {
                range.old_start
            };
            if start > pos {
                break;
            }
            let old_size = if self.inverted {
                range.new_size
            } else {
                range.old_size
            };
            let new_size = if self.inverted {
                range.old_size
            } else {
                range.new_size
            };
            if pos <= start + old_size && index == recover.index {
                return true;
            }
            diff += new_size as isize - old_size as isize;
        }
        false
    }
}

/// A chain of step maps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mapping {
    /// Step maps in order.
    pub maps: Vec<StepMap>,
    /// Pairs of map indexes that invert each other.
    #[serde(default)]
    pub mirrors: Vec<(usize, usize)>,
}

impl Mapping {
    /// Empty mapping.
    pub fn new() -> Self {
        Self {
            maps: Vec::new(),
            mirrors: Vec::new(),
        }
    }

    /// Append one map.
    pub fn append_map(&mut self, map: StepMap) {
        self.maps.push(map);
    }

    /// Return a mapping over a sub-range of maps.
    pub fn slice(&self, from: usize, to: usize) -> Self {
        let from = from.min(self.maps.len());
        let to = to.min(self.maps.len()).max(from);
        let maps = self.maps[from..to].to_vec();
        let mirrors = self
            .mirrors
            .iter()
            .filter_map(|(first, second)| {
                if (from..to).contains(first) && (from..to).contains(second) {
                    Some((first - from, second - from))
                } else {
                    None
                }
            })
            .collect();
        Self { maps, mirrors }
    }

    /// Append one map and record its mirror map index.
    pub fn append_map_with_mirror(&mut self, map: StepMap, mirror: usize) {
        let index = self.maps.len();
        self.maps.push(map);
        self.set_mirror(index, mirror);
    }

    /// Record that two maps invert each other.
    pub fn set_mirror(&mut self, first: usize, second: usize) {
        self.mirrors.push((first, second));
    }

    /// Return the mirror map index for a map index.
    pub fn get_mirror(&self, index: usize) -> Option<usize> {
        self.mirrors.iter().find_map(|(first, second)| {
            if *first == index {
                Some(*second)
            } else if *second == index {
                Some(*first)
            } else {
                None
            }
        })
    }

    /// Append all maps from another mapping, preserving backwards mirror pairs.
    pub fn append_mapping(&mut self, mapping: &Mapping) {
        let start = self.maps.len();
        for (index, map) in mapping.maps.iter().cloned().enumerate() {
            if let Some(mirror) = mapping.get_mirror(index).filter(|mirror| *mirror < index) {
                self.append_map_with_mirror(map, start + mirror);
            } else {
                self.append_map(map);
            }
        }
    }

    /// Append the inverse of another mapping.
    pub fn append_mapping_inverted(&mut self, mapping: &Mapping) {
        let start = self.maps.len();
        let len = mapping.maps.len();
        for index in (0..len).rev() {
            let map = mapping.maps[index].invert();
            if let Some(mirror) = mapping.get_mirror(index).filter(|mirror| *mirror > index) {
                self.append_map_with_mirror(map, start + len - mirror - 1);
            } else {
                self.append_map(map);
            }
        }
    }

    /// Create the inverse mapping.
    pub fn invert(&self) -> Self {
        let mut inverted = Self::new();
        inverted.append_mapping_inverted(self);
        inverted
    }

    /// Map a position through all maps.
    pub fn map(&self, pos: usize, assoc: i8) -> usize {
        if self.mirrors.is_empty() {
            Self::map_through_maps(&self.maps, pos, assoc)
        } else {
            self.map_result(pos, assoc).pos
        }
    }

    /// Map a position through a borrowed map slice without building a [`Mapping`].
    pub(crate) fn map_through_maps(maps: &[StepMap], mut pos: usize, assoc: i8) -> usize {
        for map in maps {
            pos = map.map(pos, assoc);
        }
        pos
    }

    /// Map a position and keep deletion metadata.
    pub fn map_result(&self, mut pos: usize, assoc: i8) -> MapResult {
        let mut deleted = false;
        let mut deleted_before = false;
        let mut deleted_after = false;
        let mut deleted_across = false;
        let mut index = 0;
        while index < self.maps.len() {
            let map = &self.maps[index];
            let result = map.map_result(pos, assoc);
            if let Some(recover) = result.recover
                && let Some(corresponding) = self.get_mirror(index).filter(|corresponding| {
                    *corresponding > index && *corresponding < self.maps.len()
                })
                && let Some(recovered) = self.maps[corresponding].recover(recover)
            {
                index = corresponding + 1;
                pos = recovered;
                continue;
            }
            pos = result.pos;
            deleted |= result.deleted;
            deleted_before |= result.deleted_before;
            deleted_after |= result.deleted_after;
            deleted_across |= result.deleted_across;
            index += 1;
        }
        MapResult {
            pos,
            deleted,
            deleted_before,
            deleted_after,
            deleted_across,
            recover: None,
        }
    }
}

/// A chained transform over a document.
#[derive(Clone, Debug)]
pub struct Transform {
    schema: Schema,
    doc: Node,
    docs: Vec<Node>,
    steps: Vec<Step>,
    maps: Vec<StepMap>,
}

impl Transform {
    /// Start a transform.
    pub fn new(schema: Schema, doc: Node) -> Self {
        Self {
            schema,
            doc,
            docs: Vec::new(),
            steps: Vec::new(),
            maps: Vec::new(),
        }
    }

    /// Current document.
    pub fn doc(&self) -> &Node {
        &self.doc
    }

    /// Schema used by this transform.
    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    /// Applied steps.
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Documents before each applied step.
    pub fn docs(&self) -> &[Node] {
        &self.docs
    }

    /// Maps produced by each applied step.
    pub fn maps(&self) -> &[StepMap] {
        &self.maps
    }

    /// Mapping produced by the whole transform.
    pub fn mapping(&self) -> Mapping {
        Mapping {
            maps: self.maps.clone(),
            mirrors: Vec::new(),
        }
    }

    /// Return the post-transform range covered by replacement steps, if any.
    pub fn changed_range(&self) -> Option<ChangedRange> {
        let mut from = usize::MAX;
        let mut to = 0;
        let mut found = false;

        for map in &self.maps {
            if found {
                from = map.map(from, 1);
                to = map.map(to, -1);
            }
            map.for_each(|_, _, new_from, new_to| {
                from = from.min(new_from);
                to = to.max(new_to);
                found = true;
            });
        }

        found.then_some(ChangedRange { from, to })
    }

    /// Apply a step.
    pub fn step(&mut self, step: Step) -> RichTextResult<&mut Self> {
        let before = self.doc.clone();
        let result = step.apply(&self.doc, &self.schema)?;
        self.doc = result.doc;
        self.docs.push(before);
        self.maps.push(result.map);
        self.steps.push(step);
        Ok(self)
    }

    /// Replace a range.
    pub fn replace(&mut self, from: usize, to: usize, slice: Slice) -> RichTextResult<&mut Self> {
        let outcome = fit_replace(&self.doc, from, to, slice, &self.schema)?;
        let map = StepMap::single(
            outcome.from,
            outcome.to - outcome.from,
            outcome.slice.size(),
        );
        let step = Step::Replace(ReplaceStep::new(outcome.from, outcome.to, outcome.slice));
        match outcome.applied_doc {
            Some(new_doc) => self.commit_step(step, new_doc, map),
            None => self.step(step),
        }
    }

    /// Commit a precomputed step result without re-applying the step.
    fn commit_step(
        &mut self,
        step: Step,
        new_doc: Node,
        map: StepMap,
    ) -> RichTextResult<&mut Self> {
        let before = std::mem::replace(&mut self.doc, new_doc);
        self.docs.push(before);
        self.maps.push(map);
        self.steps.push(step);
        Ok(self)
    }

    /// Replace a range with the given content.
    pub fn replace_with(
        &mut self,
        from: usize,
        to: usize,
        content: Fragment,
    ) -> RichTextResult<&mut Self> {
        self.replace(from, to, Slice::new(content, 0, 0))
    }

    /// Replace a range using the range and slice open depths as fitting hints.
    pub fn replace_range(
        &mut self,
        from: usize,
        to: usize,
        slice: Slice,
    ) -> RichTextResult<&mut Self> {
        self.replace(from, to, slice)
    }

    /// Replace a range with a node, fitting it around block boundaries when needed.
    pub fn replace_range_with(
        &mut self,
        from: usize,
        to: usize,
        node: Node,
    ) -> RichTextResult<&mut Self> {
        self.replace_range(from, to, Slice::new(Fragment::from(node), 0, 0))
    }

    /// Replace a range while preserving a gap inside the replacement.
    pub fn replace_around(
        &mut self,
        from: usize,
        to: usize,
        gap_from: usize,
        gap_to: usize,
        slice: Slice,
        insert: usize,
    ) -> RichTextResult<&mut Self> {
        self.step(Step::ReplaceAround(ReplaceAroundStep::new(
            from, to, gap_from, gap_to, slice, insert,
        )))
    }

    /// Delete a range.
    pub fn delete(&mut self, from: usize, to: usize) -> RichTextResult<&mut Self> {
        self.replace(from, to, Slice::empty())
    }

    /// Delete a range using the same fitting path as [`Transform::delete`].
    pub fn delete_range(&mut self, from: usize, to: usize) -> RichTextResult<&mut Self> {
        self.delete(from, to)
    }

    /// Insert content at a position.
    pub fn insert(&mut self, pos: usize, content: Fragment) -> RichTextResult<&mut Self> {
        self.replace_with(pos, pos, content)
    }

    /// Add a mark across an inline range.
    pub fn add_mark(&mut self, from: usize, to: usize, mark: Mark) -> RichTextResult<&mut Self> {
        self.step(Step::AddMark(MarkStep::new(from, to, mark)))
    }

    /// Remove a mark across an inline range.
    pub fn remove_mark(&mut self, from: usize, to: usize, mark: Mark) -> RichTextResult<&mut Self> {
        self.step(Step::RemoveMark(MarkStep::new(from, to, mark)))
    }

    /// Add a mark to the node at a position.
    pub fn add_node_mark(&mut self, pos: usize, mark: Mark) -> RichTextResult<&mut Self> {
        self.step(Step::AddNodeMark(NodeMarkStep { pos, mark }))
    }

    /// Remove an exact mark from the node at a position.
    pub fn remove_node_mark(&mut self, pos: usize, mark: Mark) -> RichTextResult<&mut Self> {
        let Some(node) = node_at(&self.doc, pos) else {
            return Err(RichTextError::InvalidPosition {
                pos,
                size: self.doc.content_size(),
            });
        };
        if mark.is_in_set(node.marks()) {
            self.step(Step::RemoveNodeMark(NodeMarkStep { pos, mark }))?;
        }
        Ok(self)
    }

    /// Remove all marks of a type from the node at a position.
    pub fn remove_node_mark_type(
        &mut self,
        pos: usize,
        mark_type: &str,
    ) -> RichTextResult<&mut Self> {
        let Some(node) = node_at(&self.doc, pos) else {
            return Err(RichTextError::InvalidPosition {
                pos,
                size: self.doc.content_size(),
            });
        };
        let marks = node
            .marks()
            .iter()
            .filter(|mark| mark.type_name() == mark_type)
            .cloned()
            .collect::<Vec<_>>();
        for mark in marks.into_iter().rev() {
            self.step(Step::RemoveNodeMark(NodeMarkStep { pos, mark }))?;
        }
        Ok(self)
    }

    /// Change the type, attributes, and/or marks of the node at a position.
    pub fn set_node_markup(
        &mut self,
        pos: usize,
        node_type: Option<&str>,
        attrs: Attrs,
        marks: Option<Vec<Mark>>,
    ) -> RichTextResult<&mut Self> {
        let Some(node) = node_at(&self.doc, pos) else {
            return Err(RichTextError::InvalidPosition {
                pos,
                size: self.doc.content_size(),
            });
        };
        if node.is_text() {
            return Err(RichTextError::Transform(
                "cannot set markup on a text node".to_string(),
            ));
        }

        let old_node_size = node.node_size();
        let old_is_leaf = node.is_leaf();
        let old_content = node.content().clone();
        let type_name = node_type.unwrap_or_else(|| node.type_name()).to_string();
        let node_type = self.schema.node_type(&type_name)?;
        let attrs = self.schema.resolve_node_attrs(&type_name, attrs)?;
        let marks = marks.unwrap_or_else(|| node.marks().to_vec());
        let shell = Node::unchecked(
            type_name,
            attrs,
            marks,
            Fragment::empty(),
            None,
            node_type.content_expr().is_empty(),
        );

        if old_is_leaf {
            self.replace_with(pos, pos + old_node_size, Fragment::from(shell))
        } else {
            let updated = shell.copy_with_content(old_content);
            self.schema.check_node(&updated)?;
            self.replace_around(
                pos,
                pos + old_node_size,
                pos + 1,
                pos + old_node_size - 1,
                Slice::new(Fragment::from(shell), 0, 0),
                1,
            )
        }
    }

    /// Set all textblocks touched by a range to a new node type and attributes.
    pub fn set_block_type(
        &mut self,
        from: usize,
        to: usize,
        node_type: &str,
        attrs: Attrs,
    ) -> RichTextResult<&mut Self> {
        self.set_block_type_with(from, to, node_type, |_| attrs.clone())
    }

    /// Set the block type for textblocks in a range, deriving the attributes
    /// for each target from the existing node. Matches upstream
    /// `setBlockType(from, to, type, node => attrs)`.
    pub fn set_block_type_with<F>(
        &mut self,
        from: usize,
        to: usize,
        node_type: &str,
        attrs_for: F,
    ) -> RichTextResult<&mut Self>
    where
        F: Fn(&Node) -> Attrs,
    {
        let target_type = self.schema.node_type(node_type)?;
        if !target_type.inline_content(&self.schema) {
            return Err(RichTextError::Transform(format!(
                "{node_type} is not a textblock type"
            )));
        }

        let mut targets = Vec::new();
        self.doc.nodes_between(from, to, |node, pos, _, _| {
            if self
                .schema
                .node_type(node.type_name())
                .is_ok_and(|node_type| node_type.inline_content(&self.schema))
            {
                targets.push((pos, node.clone()));
                false
            } else {
                true
            }
        })?;

        for (pos, node) in targets.into_iter().rev() {
            let attrs = attrs_for(&node);
            if node.type_name() == node_type && node.attrs() == &attrs {
                continue;
            }

            // Skip nodes whose parent's content expression won't accept the
            // new type — matches upstream's "skips nodes that can't be changed
            // due to constraints" behaviour (e.g., a list_item that requires
            // `paragraph block*` rejects making its first paragraph a
            // code_block).
            let resolved = self.doc.resolve(pos)?;
            let parent = resolved.parent();
            let parent_index = resolved.index(resolved.depth()).unwrap_or(0);
            if !parent.can_replace_with(
                parent_index,
                parent_index + 1,
                node_type,
                &[],
                &self.schema,
            ) {
                continue;
            }

            let content = fit_textblock_content(&self.schema, node_type, &attrs, node.content())?;
            let replacement = self
                .schema
                .node(node_type, attrs, content)?
                .with_marks(node.marks().to_vec());
            self.replace_with(pos, pos + node.node_size(), Fragment::from(replacement))?;
        }
        Ok(self)
    }

    /// Set an attribute on the node at a position.
    pub fn set_node_attr(
        &mut self,
        pos: usize,
        attr: impl Into<String>,
        value: Option<Value>,
    ) -> RichTextResult<&mut Self> {
        self.step(Step::Attr(AttrStep {
            pos,
            attr: attr.into(),
            value,
        }))
    }

    /// Set an attribute on the document node.
    pub fn set_doc_attr(
        &mut self,
        attr: impl Into<String>,
        value: Option<Value>,
    ) -> RichTextResult<&mut Self> {
        self.step(Step::DocAttr(DocAttrStep {
            attr: attr.into(),
            value,
        }))
    }

    /// Split the parent node at a position.
    pub fn split(&mut self, pos: usize, depth: usize) -> RichTextResult<&mut Self> {
        let new_doc = split_doc_at(&self.doc, pos, depth, &self.schema)?;
        self.replace_whole_doc(new_doc)
    }

    /// Split the parent node at a position while overriding the right-side
    /// node type at one or more split levels.
    pub fn split_into(
        &mut self,
        pos: usize,
        depth: usize,
        types_after: &[Option<TypeAfter>],
    ) -> RichTextResult<&mut Self> {
        let new_doc = split_doc_at_into(&self.doc, pos, depth, types_after, &self.schema)?;
        self.replace_whole_doc(new_doc)
    }

    /// Join sibling nodes around a boundary position.
    pub fn join(&mut self, pos: usize) -> RichTextResult<&mut Self> {
        let new_doc = join_doc_at(&self.doc, pos, &self.schema)?;
        self.replace_whole_doc(new_doc)
    }

    /// Wrap sibling content in a new parent node.
    pub fn wrap(
        &mut self,
        from: usize,
        to: usize,
        node_type: impl Into<String>,
        attrs: Attrs,
    ) -> RichTextResult<&mut Self> {
        self.wrap_chain(
            from,
            to,
            std::iter::once(WrapperSpec {
                type_name: node_type.into(),
                attrs,
            }),
        )
    }

    /// Wrap the gap `[from, to]` in a chain of wrappers — outermost
    /// first. Mirrors PM's `tr.wrap(range, wrappers)`. The whole chain
    /// gets placed by a single `ReplaceAround` step with `openStart` /
    /// `openEnd` equal to `chain.len()`, so the inner-most wrapper
    /// absorbs the gap content. Use this (not `wrap`) when the schema
    /// requires intermediate wrappers between the gap parent and the
    /// target — e.g. wrapping paragraphs in `bullet_list` requires the
    /// `list_item` step between, returned by `find_wrapping`.
    pub fn wrap_chain<I>(&mut self, from: usize, to: usize, chain: I) -> RichTextResult<&mut Self>
    where
        I: IntoIterator<Item = WrapperSpec>,
    {
        let chain: Vec<WrapperSpec> = chain.into_iter().collect();
        if chain.is_empty() {
            return Err(RichTextError::Transform(
                "wrap_chain requires at least one wrapper".to_string(),
            ));
        }

        let from_resolved = self.doc.resolve(from)?;
        let to_resolved = self.doc.resolve(to)?;
        let depth = from_resolved.shared_depth(to);
        let start_index =
            from_resolved
                .index(depth)
                .ok_or_else(|| RichTextError::InvalidPosition {
                    pos: from,
                    size: self.doc.content_size(),
                })?;
        let end_index =
            to_resolved
                .index_after(depth)
                .ok_or_else(|| RichTextError::InvalidPosition {
                    pos: to,
                    size: self.doc.content_size(),
                })?;
        if start_index >= end_index {
            return Err(RichTextError::Transform(
                "wrap range does not cover sibling nodes".to_string(),
            ));
        }

        let start_pos = from_resolved
            .pos_at_index(start_index, depth)
            .ok_or_else(|| RichTextError::InvalidPosition {
                pos: from,
                size: self.doc.content_size(),
            })?;
        let end_pos = from_resolved
            .pos_at_index(end_index, depth)
            .ok_or_else(|| RichTextError::InvalidPosition {
                pos: to,
                size: self.doc.content_size(),
            })?;

        // Build the wrapper chain from the inside out so each outer
        // wrapper's only child is the next wrapper down. The innermost
        // wrapper is empty — the ReplaceAround step's `openStart` /
        // `openEnd` markers tell the fitter to splice the gap content
        // there, so we never have to construct a wrapper that holds
        // the existing paragraphs ourselves.
        let mut wrapper: Option<Node> = None;
        for spec in chain.iter().rev() {
            let wrapper_type = self.schema.node_type(&spec.type_name)?;
            let attrs = self
                .schema
                .resolve_node_attrs(&spec.type_name, spec.attrs.clone())?;
            let content = match wrapper.take() {
                None => Fragment::empty(),
                Some(inner) => Fragment::from(inner),
            };
            wrapper = Some(Node::unchecked(
                spec.type_name.clone(),
                attrs,
                Vec::new(),
                content,
                None,
                wrapper_type.content_expr().is_empty(),
            ));
        }
        let outer = wrapper.expect("non-empty chain produces an outer wrapper");
        // PM's `wrap(range, wrappers)`: the slice is closed on both
        // sides (`openStart = openEnd = 0`); the `insert` field of the
        // ReplaceAround step tells the step which depth inside the
        // chain to splice the original gap content at.
        let slice = Slice::new(Fragment::from(outer), 0, 0);

        self.step(Step::ReplaceAround(ReplaceAroundStep {
            from: start_pos,
            to: end_pos,
            gap_from: start_pos,
            gap_to: end_pos,
            slice,
            insert: chain.len(),
            structure: true,
        }))
    }

    /// Lift a whole wrapper node's selected content into its grandparent.
    pub fn lift(&mut self, from: usize, to: usize) -> RichTextResult<&mut Self> {
        if from > to {
            return Err(RichTextError::Transform(
                "lift range start must not be after end".to_string(),
            ));
        }

        let from_resolved = self.doc.resolve(from)?;
        let to_resolved = self.doc.resolve(to)?;
        let range = from_resolved
            .block_range(Some(&to_resolved))
            .ok_or_else(|| RichTextError::Transform("range cannot be lifted".to_string()))?;
        if range.depth() == 0 {
            return Err(RichTextError::Transform(
                "cannot lift top-level content".to_string(),
            ));
        }
        if range.start_index() >= range.end_index() {
            return Err(RichTextError::Transform(
                "lift range does not cover sibling nodes".to_string(),
            ));
        }

        // Walk outward through ancestors, but stop at the deepest isolating
        // ancestor — upstream's `liftTarget` does the same so e.g. lifting a
        // paragraph out of a `table_cell` (isolating) doesn't escape the cell.
        let max_target = max_lift_target_depth(&from_resolved, &range, &self.schema)?;
        for target_depth in (max_target..range.depth()).rev() {
            let Ok(step) = build_lift_around_step(
                &from_resolved,
                &to_resolved,
                &range,
                target_depth,
                &self.schema,
            ) else {
                continue;
            };
            if self.step(Step::ReplaceAround(step)).is_ok() {
                return Ok(self);
            }
        }

        let new_doc = lift_doc_range(&self.doc, from, to, &self.schema)?;
        self.replace_whole_doc(new_doc)
    }

    fn replace_whole_doc(&mut self, new_doc: Node) -> RichTextResult<&mut Self> {
        let size = self.doc.content_size();
        self.replace(0, size, Slice::new(new_doc.content().clone(), 0, 0))
    }
}

/// Check whether a split would be valid.
pub fn can_split(doc: &Node, pos: usize, depth: usize, schema: &Schema) -> bool {
    can_split_into(doc, pos, depth, &[], schema)
}

/// Check whether splitting at `pos` to `depth` is allowed, with the right
/// halves of each split level optionally taking on a different node type
/// (`types_after[i]` is the type the level `depth - 1 - i` should become after
/// the split; `None` keeps the existing type). Mirrors upstream's
/// `canSplit(doc, pos, depth?, typesAfter?)`.
pub fn can_split_into(
    doc: &Node,
    pos: usize,
    depth: usize,
    types_after: &[Option<TypeAfter>],
    schema: &Schema,
) -> bool {
    let Ok(resolved) = doc.resolve(pos) else {
        return false;
    };
    let parent = resolved.parent();
    let parent_index = resolved.index(resolved.depth()).unwrap_or(0);
    let parent_type = match schema.node_type(parent.type_name()) {
        Ok(t) => t,
        Err(_) => return false,
    };
    if parent_type.is_isolating() {
        return false;
    }
    if !parent.can_replace(
        parent_index,
        parent.child_count(),
        &Fragment::empty(),
        schema,
    ) {
        return false;
    }

    // innerType: the type the deepest right-side node should become.
    let inner_type_owned;
    let inner_type: &NodeType = if let Some(Some(spec)) = types_after.last() {
        match schema.node_type(&spec.type_name) {
            Ok(t) => t,
            Err(_) => return false,
        }
    } else {
        inner_type_owned = parent_type.clone();
        &inner_type_owned
    };
    let parent_right = parent
        .content()
        .cut_by_index(parent_index, parent.child_count());
    if !inner_type
        .content_expr()
        .matches_fragment(schema, &parent_right)
    {
        return false;
    }

    let base = match resolved.depth().checked_sub(depth) {
        Some(v) => v,
        None => return false,
    };

    // Walk inward levels above the cursor parent.
    let mut d = resolved.depth().saturating_sub(1);
    let mut i = depth.saturating_sub(2) as isize;
    while d > base {
        let Some(node) = resolved.node(d) else {
            return false;
        };
        let Ok(node_type) = schema.node_type(node.type_name()) else {
            return false;
        };
        if node_type.is_isolating() {
            return false;
        }
        let index = resolved.index(d).unwrap_or(0);
        let mut rest = node.content().cut_by_index(index, node.child_count());
        let override_child = type_after_at(types_after, (i + 1) as usize);
        if let Some(spec) = override_child {
            let Ok(target) = schema.node_type(&spec.type_name) else {
                return false;
            };
            let Ok(replacement) = schema.node(target.name(), spec.attrs.clone(), Fragment::empty())
            else {
                return false;
            };
            let Ok(updated) = rest.replace_child(0, replacement) else {
                return false;
            };
            rest = updated;
        }
        let after_type_owned;
        let after_type: &NodeType = if let Some(spec) = type_after_at(types_after, i as usize) {
            match schema.node_type(&spec.type_name) {
                Ok(t) => t,
                Err(_) => return false,
            }
        } else {
            after_type_owned = node_type.clone();
            &after_type_owned
        };
        if !node.can_replace(index + 1, node.child_count(), &Fragment::empty(), schema) {
            return false;
        }
        if !after_type.content_expr().matches_fragment(schema, &rest) {
            return false;
        }
        if d == 0 {
            break;
        }
        d -= 1;
        i -= 1;
    }

    let Some(base_node) = resolved.node(base) else {
        return false;
    };
    let index_after_base = resolved.index_after(base).unwrap_or(0);
    let base_type_name = if let Some(spec) = type_after_at(types_after, 0) {
        spec.type_name.clone()
    } else {
        let Some(below) = resolved.node(base + 1) else {
            return false;
        };
        below.type_name().to_string()
    };
    base_node.can_replace_with(
        index_after_base,
        index_after_base,
        &base_type_name,
        &[],
        schema,
    )
}

/// Optional override for the right-side node type at a split level.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeAfter {
    /// Name of the node type the right half should become.
    pub type_name: String,
    /// Attributes for the new node.
    pub attrs: Attrs,
}

impl TypeAfter {
    /// Construct an override with no attributes.
    pub fn new(type_name: impl Into<String>) -> Self {
        Self {
            type_name: type_name.into(),
            attrs: Attrs::new(),
        }
    }
}

fn type_after_at(types_after: &[Option<TypeAfter>], index: usize) -> Option<&TypeAfter> {
    types_after.get(index).and_then(|slot| slot.as_ref())
}

/// Try to find a target depth a `NodeRange`'s content can be lifted to. Walks
/// outward from the range's depth, stopping at isolating ancestors or where the
/// parent can no longer be cut to make room for the lifted content.
///
/// Mirrors upstream `liftTarget(range)`.
pub fn lift_target(range: &NodeRange, schema: &Schema) -> Option<usize> {
    let parent = range.parent();
    let content = parent
        .content()
        .cut_by_index(range.start_index(), range.end_index());
    let from = range.from_resolved();
    let to = range.to_resolved();
    let mut content_before: usize = 0;
    let mut content_after: usize = 0;
    let mut depth = range.depth();
    loop {
        let node = from.node(depth)?;
        let node_type = schema.node_type(node.type_name()).ok()?;
        let index = from.index(depth)?.saturating_add(content_before);
        let to_index = to
            .index_after(depth)
            .unwrap_or(node.child_count())
            .saturating_sub(content_after);
        if depth < range.depth() && node.can_replace(index, to_index, &content, schema) {
            return Some(depth);
        }
        if depth == 0 || node_type.is_isolating() || !can_cut_in(node, index, to_index, schema) {
            break;
        }
        if index > 0 {
            content_before = 1;
        }
        if to_index < node.child_count() {
            content_after = 1;
        }
        depth -= 1;
    }
    None
}

fn can_cut_in(node: &Node, start: usize, end: usize, schema: &Schema) -> bool {
    let child_count = node.child_count();
    (start == 0 || node.can_replace(start, child_count, &Fragment::empty(), schema))
        && (end == child_count || node.can_replace(0, end, &Fragment::empty(), schema))
}

/// Try to find a valid way to wrap a `NodeRange` in a node of the given type.
/// Returns a chain of wrapper specs that, nested around the range's content,
/// satisfies both the range parent's content match (around) and the outermost
/// wrapper's content match (inside). Mirrors upstream `findWrapping(range,
/// nodeType, attrs?, innerRange?)`. Pine accepts only the simple two-arg form
/// for now — every wrapper in the returned chain has empty attrs except the
/// outermost, which carries the caller's `attrs`.
pub fn find_wrapping(
    range: &NodeRange,
    node_type: &NodeType,
    attrs: Attrs,
    schema: &Schema,
) -> Option<Vec<WrapperSpec>> {
    let outside = find_wrapping_outside(range, node_type, schema)?;
    let inside = find_wrapping_inside(range, node_type, schema)?;
    let mut result = Vec::with_capacity(outside.len() + 1 + inside.len());
    for t in &outside {
        result.push(WrapperSpec {
            type_name: t.name().to_string(),
            attrs: Attrs::new(),
        });
    }
    result.push(WrapperSpec {
        type_name: node_type.name().to_string(),
        attrs,
    });
    for t in &inside {
        result.push(WrapperSpec {
            type_name: t.name().to_string(),
            attrs: Attrs::new(),
        });
    }
    Some(result)
}

/// A wrapper layer returned by `find_wrapping`.
#[derive(Clone, Debug, PartialEq)]
pub struct WrapperSpec {
    /// Name of the wrapper node type.
    pub type_name: String,
    /// Attributes to apply when constructing the wrapper.
    pub attrs: Attrs,
}

fn find_wrapping_outside(
    range: &NodeRange,
    node_type: &NodeType,
    schema: &Schema,
) -> Option<Vec<NodeType>> {
    let parent = range.parent();
    let around = parent
        .content_match_at(range.start_index(), schema)
        .ok()?
        .find_wrapping(node_type)?;
    let outer_name = around
        .first()
        .map(|t| t.name())
        .unwrap_or_else(|| node_type.name());
    if !parent.can_replace_with(
        range.start_index(),
        range.end_index(),
        outer_name,
        &[],
        schema,
    ) {
        return None;
    }
    // schema lookups → owned NodeType clones (the chain types may not all live
    // long enough to borrow from `around`'s caller scope).
    let _ = schema;
    Some(around)
}

fn find_wrapping_inside(
    range: &NodeRange,
    node_type: &NodeType,
    schema: &Schema,
) -> Option<Vec<NodeType>> {
    let parent = range.parent();
    let inner = parent.content().child(range.start_index())?;
    let inner_type = schema.node_type(inner.type_name()).ok()?;
    let inside = node_type
        .content_expr()
        .match_fragment(schema, &Fragment::empty())?
        .find_wrapping(inner_type)?;
    let last_type_owned;
    let last_type: &NodeType = if let Some(last) = inside.last() {
        last_type_owned = last.clone();
        &last_type_owned
    } else {
        node_type
    };
    let mut inner_match = last_type
        .content_expr()
        .match_fragment(schema, &Fragment::empty())?;
    for i in range.start_index()..range.end_index() {
        let child = parent.content().child(i)?;
        let child_type = schema.node_type(child.type_name()).ok()?;
        inner_match = inner_match.match_type(child_type)?;
    }
    if !inner_match.valid_end() {
        return None;
    }
    Some(inside)
}

/// Check whether sibling nodes around a boundary can be joined.
pub fn can_join(doc: &Node, pos: usize, schema: &Schema) -> bool {
    join_doc_at(doc, pos, schema).is_ok()
}

/// Find a nearby joinable boundary, searching outward through ancestors.
pub fn join_point(doc: &Node, pos: usize, dir: i8, schema: &Schema) -> Option<usize> {
    let resolved = doc.resolve(pos).ok()?;
    let dir = if dir < 0 { -1 } else { 1 };
    let mut candidate = pos;
    for depth in (0..=resolved.depth()).rev() {
        if can_join(doc, candidate, schema) {
            return Some(candidate);
        }
        if depth == 0 {
            break;
        }
        candidate = if dir < 0 {
            resolved.before(depth)?
        } else {
            resolved.after(depth)?
        };
    }
    None
}

/// Result of running the replace fitters. When `applied_doc` is `Some`, the
/// happy-path replace has already produced the new document, so the caller can
/// commit the step without re-running `replace`.
struct FitOutcome {
    from: usize,
    to: usize,
    slice: Slice,
    applied_doc: Option<Node>,
}

fn fit_replace(
    doc: &Node,
    from: usize,
    to: usize,
    slice: Slice,
    schema: &Schema,
) -> RichTextResult<FitOutcome> {
    // The defining-context drop must run before the plain replace, otherwise
    // inserting a wrapped slice into an empty paragraph silently keeps the
    // paragraph and unwraps the slice's outer node.
    if let Some((from, to, slice, applied_doc)) =
        fit_empty_parent_with_wrapped_slice(doc, from, to, &slice, schema)?
    {
        return Ok(FitOutcome {
            from,
            to,
            slice,
            applied_doc: Some(applied_doc),
        });
    }
    // The defining-context merge handles a non-empty cursor parent where the
    // slice carries a defining_for_content wrapper. Like the empty-parent
    // drop, this MUST run before the plain replace so the wrapper survives.
    if let Some((from, to, slice, applied_doc)) =
        fit_defining_context_merge(doc, from, to, &slice, schema)?
    {
        return Ok(FitOutcome {
            from,
            to,
            slice,
            applied_doc: Some(applied_doc),
        });
    }
    if let Ok(applied) = replace(doc, from, to, slice.clone(), schema) {
        return Ok(FitOutcome {
            from,
            to,
            slice,
            applied_doc: Some(applied),
        });
    }
    // The bare replace just rejected the slice — likely because openStart
    // pierces deeper than from.depth allows. Try to close the slice's open
    // top-level wrapper(s) by auto-filling their missing prefix, then retry.
    if let Some((from, to, slice, applied_doc)) =
        fit_close_open_start_with_autofill(doc, from, to, &slice, schema)?
    {
        return Ok(FitOutcome {
            from,
            to,
            slice,
            applied_doc: Some(applied_doc),
        });
    }
    if let Some((from, to, slice, applied_doc)) =
        fit_block_slice_inside_textblock(doc, from, to, &slice, schema)?
    {
        return Ok(FitOutcome {
            from,
            to,
            slice,
            applied_doc: Some(applied_doc),
        });
    }
    if let Some((from, to, slice, applied_doc)) =
        fit_inline_slice_inside_block_parent(doc, from, to, &slice, schema)?
    {
        return Ok(FitOutcome {
            from,
            to,
            slice,
            applied_doc: Some(applied_doc),
        });
    }
    if let Some((from, to, slice, applied_doc)) =
        fit_cross_depth_empty_slice(doc, from, to, &slice, schema)?
    {
        return Ok(FitOutcome {
            from,
            to,
            slice,
            applied_doc: Some(applied_doc),
        });
    }
    Ok(FitOutcome {
        from,
        to,
        slice,
        applied_doc: None,
    })
}

fn fit_empty_parent_with_wrapped_slice(
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
    schema: &Schema,
) -> RichTextResult<Option<(usize, usize, Slice, Node)>> {
    // When the cursor sits inside an empty textblock and the slice carries its
    // own outer wrapper (open_start/open_end ≥ 1 with a single top-level
    // child), upstream's replaceRange unwraps the cursor's parent and uses the
    // slice's wrapper type instead. This is the "defining context drop" case
    // for replacing an empty paragraph with the slice's wrapper.
    if from != to {
        return Ok(None);
    }
    if slice.content.len() != 1 || slice.open_start == 0 || slice.open_end == 0 {
        return Ok(None);
    }

    let resolved = doc.resolve(from)?;
    if resolved.depth() == 0 {
        return Ok(None);
    }
    if resolved.parent().content_size() != 0 {
        return Ok(None);
    }

    let wrapper = slice
        .content
        .child(0)
        .ok_or_else(|| RichTextError::Transform("missing slice wrapper".to_string()))?
        .clone();
    let grand_depth = resolved.depth() - 1;
    let grand = resolved.node(grand_depth).ok_or_else(|| {
        RichTextError::Transform("missing grand node for defining-context drop".to_string())
    })?;
    let parent_index = resolved.index(grand_depth).ok_or_else(|| {
        RichTextError::Transform("missing parent index for defining-context drop".to_string())
    })?;
    if !grand.can_replace_with(
        parent_index,
        parent_index + 1,
        wrapper.type_name(),
        &[],
        schema,
    ) {
        return Ok(None);
    }

    let new_from = resolved.before(resolved.depth()).ok_or_else(|| {
        RichTextError::Transform("missing parent start for defining-context drop".to_string())
    })?;
    let new_to = resolved.after(resolved.depth()).ok_or_else(|| {
        RichTextError::Transform("missing parent end for defining-context drop".to_string())
    })?;
    let new_slice = Slice::new(Fragment::from(wrapper), 0, 0);

    match replace(doc, new_from, new_to, new_slice.clone(), schema) {
        Ok(applied) => Ok(Some((new_from, new_to, new_slice, applied))),
        Err(_) => Ok(None),
    }
}

/// When the cursor sits inside a non-empty textblock and the inserted slice
/// carries a `defining_for_content` wrapper inside its open chain, replace the
/// whole cursor parent with the slice — but first merge the cursor parent's
/// head/tail content into the slice's leftmost/rightmost open textblocks so
/// nothing is lost. Mirrors upstream's "keeps defining context when inserting
/// at the start of a textblock" replaceRange case.
fn fit_defining_context_merge(
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
    schema: &Schema,
) -> RichTextResult<Option<(usize, usize, Slice, Node)>> {
    if slice.open_start == 0 || slice.open_end == 0 || slice.content.is_empty() {
        return Ok(None);
    }

    let from_resolved = doc.resolve(from)?;
    let to_resolved = doc.resolve(to)?;
    if from_resolved.depth() == 0 || to_resolved.depth() == 0 {
        return Ok(None);
    }
    // Only same-depth ranges. Cross-depth cases need different bookkeeping.
    if from_resolved.depth() != to_resolved.depth() {
        return Ok(None);
    }

    let from_parent = from_resolved.parent();
    let to_parent = to_resolved.parent();
    let from_parent_type = schema.node_type(from_parent.type_name())?;
    let to_parent_type = schema.node_type(to_parent.type_name())?;
    if !from_parent_type.inline_content(schema) || !to_parent_type.inline_content(schema) {
        return Ok(None);
    }

    let same_parent = from_resolved.same_parent(&to_resolved);
    // Empty-parent (cursor with no surrounding text) is the empty-parent
    // fitter's job — but only when from == to. For a range that spans the
    // whole textblock, the merge still needs to happen.
    if from == to && from_parent.content_size() == 0 {
        return Ok(None);
    }
    if !slice_open_chain_has_defining_wrapper(slice, schema)? {
        return Ok(None);
    }

    // head = content of the from-side textblock before the from cursor.
    // tail = content of the to-side textblock after the to cursor.
    let head_content = from_parent
        .content()
        .cut(0, from_resolved.parent_offset())?;
    let tail_content = to_parent
        .content()
        .cut(to_resolved.parent_offset(), to_parent.content_size())?;

    // Merge head into the slice's leftmost leaf, tail into the rightmost leaf.
    let mut merged = slice.content.clone();
    if !head_content.is_empty() {
        let leaf = left_leaf(&merged, slice.open_start).ok_or_else(|| {
            RichTextError::Transform("missing left leaf for defining merge".to_string())
        })?;
        let new_leaf_content = head_content.append(leaf.content().clone());
        merged = replace_left_leaf_content(&merged, slice.open_start, new_leaf_content)?;
    }
    if !tail_content.is_empty() {
        let leaf = right_leaf(&merged, slice.open_end).ok_or_else(|| {
            RichTextError::Transform("missing right leaf for defining merge".to_string())
        })?;
        let new_leaf_content = leaf.content().clone().append(tail_content);
        merged = replace_right_leaf_content(&merged, slice.open_end, new_leaf_content)?;
    }

    // Close both sides — the slice is now fully populated.
    let merged_slice = Slice::new(merged, 0, 0);

    // Range to replace:
    //   same parent: [before(parent) .. after(parent)] — wipe the whole textblock.
    //   different parents at the same depth: [before(from_parent) .. after(to_parent)] —
    //     remove everything from the from-side textblock through the to-side textblock.
    let new_from = from_resolved.before(from_resolved.depth()).ok_or_else(|| {
        RichTextError::Transform("missing parent start for defining merge".to_string())
    })?;
    let new_to = if same_parent {
        from_resolved.after(from_resolved.depth())
    } else {
        to_resolved.after(to_resolved.depth())
    }
    .ok_or_else(|| RichTextError::Transform("missing parent end for defining merge".to_string()))?;

    match replace(doc, new_from, new_to, merged_slice.clone(), schema) {
        Ok(applied) => Ok(Some((new_from, new_to, merged_slice, applied))),
        Err(_) => Ok(None),
    }
}

/// Walk the slice's leftmost open chain (`open_start` levels deep) and report
/// whether any node along the way is declared `defining_for_content`.
fn slice_open_chain_has_defining_wrapper(slice: &Slice, schema: &Schema) -> RichTextResult<bool> {
    let mut current = slice.content.child(0);
    for _ in 0..slice.open_start {
        let Some(node) = current else { break };
        if schema
            .node_type(node.type_name())?
            .is_defining_for_content()
        {
            return Ok(true);
        }
        current = node.content().child(0);
    }
    // Also check the right side — a defining wrapper anywhere in the chain
    // counts. For a typical slice (single chain) this is the same walk.
    let mut current = slice.content.as_slice().last();
    for _ in 0..slice.open_end {
        let Some(node) = current else { break };
        if schema
            .node_type(node.type_name())?
            .is_defining_for_content()
        {
            return Ok(true);
        }
        current = node.content().as_slice().last();
    }
    Ok(false)
}

/// Walk into `content` via the FIRST child of each level `depth` times and
/// return the resulting leaf node. `depth = 1` returns `content.child(0)`.
fn left_leaf(content: &Fragment, depth: usize) -> Option<&Node> {
    if depth == 0 {
        return None;
    }
    let mut node = content.child(0)?;
    for _ in 1..depth {
        node = node.content().child(0)?;
    }
    Some(node)
}

/// Walk via the LAST child `depth` times.
fn right_leaf(content: &Fragment, depth: usize) -> Option<&Node> {
    if depth == 0 {
        return None;
    }
    let mut node = content.as_slice().last()?;
    for _ in 1..depth {
        node = node.content().as_slice().last()?;
    }
    Some(node)
}

/// Replace the leftmost leaf's content. `depth = 1` replaces `content.child(0)`'s
/// content; deeper values recurse into the first child's content `depth - 1`
/// more times.
fn replace_left_leaf_content(
    content: &Fragment,
    depth: usize,
    new_content: Fragment,
) -> RichTextResult<Fragment> {
    if content.is_empty() || depth == 0 {
        return Err(RichTextError::Transform(
            "cannot replace left leaf at depth 0".to_string(),
        ));
    }
    let first = content
        .child(0)
        .expect("non-empty fragment has a first child");
    if depth == 1 {
        let modified = first.copy_with_content(new_content);
        return content.replace_child(0, modified);
    }
    let inner = replace_left_leaf_content(first.content(), depth - 1, new_content)?;
    let modified = first.copy_with_content(inner);
    content.replace_child(0, modified)
}

/// Replace the rightmost leaf's content. Symmetric to `replace_left_leaf_content`.
fn replace_right_leaf_content(
    content: &Fragment,
    depth: usize,
    new_content: Fragment,
) -> RichTextResult<Fragment> {
    if content.is_empty() || depth == 0 {
        return Err(RichTextError::Transform(
            "cannot replace right leaf at depth 0".to_string(),
        ));
    }
    let last_index = content.len() - 1;
    let last = content
        .child(last_index)
        .expect("non-empty fragment has a last child");
    if depth == 1 {
        let modified = last.copy_with_content(new_content);
        return content.replace_child(last_index, modified);
    }
    let inner = replace_right_leaf_content(last.content(), depth - 1, new_content)?;
    let modified = last.copy_with_content(inner);
    content.replace_child(last_index, modified)
}

fn fit_cross_depth_empty_slice(
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
    schema: &Schema,
) -> RichTextResult<Option<(usize, usize, Slice, Node)>> {
    // Only handles the empty-slice case where the endpoints sit inside
    // textblocks at different depths. Mirrors the upstream "merge text down
    // from nested nodes" and "merge text up into nested nodes" Fitter cases.
    if !slice.content.is_empty() || slice.open_start != 0 || slice.open_end != 0 {
        return Ok(None);
    }

    let from_resolved = doc.resolve(from)?;
    let to_resolved = doc.resolve(to)?;
    if from_resolved.depth() == to_resolved.depth() {
        return Ok(None);
    }

    let from_parent = from_resolved.parent();
    let to_parent = to_resolved.parent();
    if !schema
        .node_type(from_parent.type_name())?
        .inline_content(schema)
        || !schema
            .node_type(to_parent.type_name())?
            .inline_content(schema)
    {
        return Ok(None);
    }

    let shared_depth = from_resolved.shared_depth(to);

    // PM's Fitter refuses to join across an isolating boundary. Mirror that: if
    // either path between the shared ancestor (exclusive) and the endpoint's
    // parent textblock (inclusive) crosses an isolating wrapper, the merge would
    // either delete or modify content across that boundary.
    if path_crosses_isolating(&from_resolved, shared_depth, from_resolved.depth(), schema)?
        || path_crosses_isolating(&to_resolved, shared_depth, to_resolved.depth(), schema)?
    {
        return Ok(None);
    }

    // Left side wins: merged textblock takes from's type, attrs, and marks.
    let left_text = from_parent
        .content()
        .cut(0, from_resolved.parent_offset())?;
    let right_text = to_parent
        .content()
        .cut(to_resolved.parent_offset(), to_parent.content_size())?;
    let merged_content = left_text.append(right_text);

    let mut wrapped = from_parent.copy_with_content(merged_content);
    for depth in (shared_depth + 1..from_resolved.depth()).rev() {
        let ancestor = from_resolved.node(depth).ok_or_else(|| {
            RichTextError::Transform("missing ancestor for cross-depth merge".to_string())
        })?;
        let cut_index = from_resolved.index(depth).unwrap_or(0);
        let mut children: Vec<Node> = ancestor.content().as_slice()[..cut_index].to_vec();
        children.push(wrapped);
        wrapped = ancestor.copy_with_content(Fragment::from(children));
    }

    let new_from = from_resolved.before(shared_depth + 1).unwrap_or(from);
    let new_to = to_resolved.after(shared_depth + 1).unwrap_or(to);
    let new_slice = Slice::new(Fragment::from(wrapped), 0, 0);

    // Only adopt this fit when the synthesized replace is actually valid; this
    // avoids handing back a step the schema would reject in the second pass.
    match replace(doc, new_from, new_to, new_slice.clone(), schema) {
        Ok(applied) => Ok(Some((new_from, new_to, new_slice, applied))),
        Err(_) => Ok(None),
    }
}

fn fit_block_slice_inside_textblock(
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
    schema: &Schema,
) -> RichTextResult<Option<(usize, usize, Slice, Node)>> {
    if slice.open_start != 0 || slice.open_end != 0 || slice.content.is_empty() {
        return Ok(None);
    }
    let replacement_is_block = slice.content.iter().all(|node| {
        schema
            .node_type(node.type_name())
            .is_ok_and(|node_type| !node_type.is_inline())
    });
    if !replacement_is_block {
        return Ok(None);
    }

    let from_resolved = doc.resolve(from)?;
    let to_resolved = doc.resolve(to)?;
    if !from_resolved.same_parent(&to_resolved) || from_resolved.depth() == 0 {
        return Ok(None);
    }
    let parent = from_resolved.parent();
    if !schema.node_type(parent.type_name())?.inline_content(schema) {
        return Ok(None);
    }

    let depth = from_resolved.depth();
    let start = from_resolved
        .start(depth)
        .ok_or_else(|| replace_error("missing textblock start"))?;
    let before = from_resolved
        .before(depth)
        .ok_or_else(|| replace_error("missing textblock boundary"))?;
    let after = from_resolved
        .after(depth)
        .ok_or_else(|| replace_error("missing textblock boundary"))?;
    let left = parent.copy_with_content(parent.content().cut(0, from - start)?);
    let right = parent.copy_with_content(parent.content().cut(to - start, parent.content_size())?);

    let mut children = Vec::with_capacity(slice.content.len() + 2);
    if left.content_size() > 0 {
        children.push(left);
    }
    children.extend(slice.content.iter().cloned());
    if right.content_size() > 0 {
        children.push(right);
    }
    let fitted = Slice::new(Fragment::from(children), 0, 0);

    match replace(doc, before, after, fitted.clone(), schema) {
        Ok(applied) => Ok(Some((before, after, fitted, applied))),
        Err(_) => Ok(None),
    }
}

/// When a slice's `open_start` pierces deeper than the destination's depth
/// can accommodate (PM rejects with "Inserted content deeper than insertion
/// position"), try to close the slice's top-level open wrapper by
/// auto-filling its missing prefix and retry the replace.
///
/// PM's `Transform.replace` succeeds here by routing through a `Fitter` class
/// that synthesizes a fitted slice. Pine doesn't have a full Fitter
/// equivalent yet; this targeted fitter handles the common case where the
/// slice has `open_start = 1` and the open wrapper's `caption figureimage`-
/// style content is just missing its required prefix.
fn fit_close_open_start_with_autofill(
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
    schema: &Schema,
) -> RichTextResult<Option<(usize, usize, Slice, Node)>> {
    // Only handle the open_start = 1 case for now; deeper opens need a
    // recursive close that isn't worth building until a test exercises it.
    if slice.open_start != 1 || slice.content.is_empty() {
        return Ok(None);
    }
    let from_resolved = doc.resolve(from)?;
    if slice.open_start <= from_resolved.depth() {
        // Plain replace already accepts this — not our case.
        return Ok(None);
    }

    let outer = slice
        .content
        .child(0)
        .ok_or_else(|| RichTextError::Transform("missing slice wrapper".to_string()))?;
    if outer.is_text() || outer.is_leaf() {
        return Ok(None);
    }
    let outer_type = schema.node_type(outer.type_name())?;
    let outer_content = outer.content().clone();

    // Auto-fill the wrapper's content to satisfy its content match. fill_before
    // with `to_end = true` produces the minimal prefix that, prepended to the
    // existing content, makes the wrapper closed.
    let prefix = outer_type
        .content_expr()
        .fill_before(schema, &outer_content, true)
        .ok_or_else(|| {
            RichTextError::Transform("cannot auto-fill slice's open wrapper".to_string())
        })?;
    if prefix.is_empty() && outer_content.is_empty() {
        return Ok(None);
    }

    // Replace the outer wrapper's leaf content with the auto-filled version.
    // `replace_left_leaf_content` is the same helper that powers
    // `fit_defining_context_merge`; it generalizes to deeper `open_start`
    // values when this fitter grows beyond the open_start = 1 case.
    let merged_content = prefix.append(outer_content);
    let content = replace_left_leaf_content(&slice.content, slice.open_start, merged_content)?;
    let new_slice = Slice::new(content, slice.open_start - 1, slice.open_end);

    match replace(doc, from, to, new_slice.clone(), schema) {
        Ok(applied) => Ok(Some((from, to, new_slice, applied))),
        Err(_) => Ok(None),
    }
}

fn fit_inline_slice_inside_block_parent(
    doc: &Node,
    from: usize,
    to: usize,
    slice: &Slice,
    schema: &Schema,
) -> RichTextResult<Option<(usize, usize, Slice, Node)>> {
    if slice.open_start != 0 || slice.open_end != 0 || slice.content.is_empty() {
        return Ok(None);
    }
    if !slice.content.iter().all(|node| {
        schema
            .node_type(node.type_name())
            .is_ok_and(|node_type| node_type.is_inline())
    }) {
        return Ok(None);
    }

    let from_resolved = doc.resolve(from)?;
    let depth = from_resolved.shared_depth(to);
    let parent = from_resolved
        .node(depth)
        .ok_or_else(|| replace_error("missing replacement parent"))?;
    if schema.node_type(parent.type_name())?.inline_content(schema) {
        return Ok(None);
    }

    let Some(wrapper_type) = schema.inline_wrapper_type() else {
        return Ok(None);
    };
    let wrapped = schema.node(wrapper_type.name(), Attrs::new(), slice.content.clone())?;
    let fitted = Slice::new(Fragment::from(wrapped), 0, 0);

    match replace(doc, from, to, fitted.clone(), schema) {
        Ok(applied) => Ok(Some((from, to, fitted, applied))),
        Err(_) => Ok(None),
    }
}

fn fit_textblock_content(
    schema: &Schema,
    node_type: &str,
    attrs: &Attrs,
    content: &Fragment,
) -> RichTextResult<Fragment> {
    // First, apply the schema's linebreak-replacement conversion when the
    // target accepts the replacement node but the source had newline text
    // (or vice versa).
    let converted = convert_linebreak_replacement(schema, node_type, content)?;

    if schema
        .node(node_type, attrs.clone(), converted.clone())
        .is_ok()
    {
        return Ok(converted);
    }

    let cleaned = Fragment::from(
        converted
            .iter()
            .filter(|child| child.is_text())
            .map(|child| {
                let mut text = child.clone();
                text.marks = Vec::new();
                text
            })
            .collect::<Vec<_>>(),
    );
    schema.node(node_type, attrs.clone(), cleaned.clone())?;
    Ok(cleaned)
}

fn convert_linebreak_replacement(
    schema: &Schema,
    node_type: &str,
    content: &Fragment,
) -> RichTextResult<Fragment> {
    let Some(linebreak) = schema.linebreak_replacement_type() else {
        return Ok(content.clone());
    };
    let linebreak_name = linebreak.name();
    let linebreak_probe = schema.default_node(linebreak_name)?;
    let probe_fragment = Fragment::from(linebreak_probe.clone());
    let target = schema.node_type(node_type)?;
    let target_accepts_linebreak = target
        .content_expr()
        .matches_fragment(schema, &probe_fragment);
    let target_is_pre = target.whitespace() == crate::model::Whitespace::Pre;

    let mut has_newline_text = false;
    let mut has_linebreak_node = false;
    for child in content.iter() {
        if child.text().is_some_and(|text| text.contains('\n')) {
            has_newline_text = true;
        }
        if child.type_name() == linebreak_name {
            has_linebreak_node = true;
        }
    }

    // Mirror upstream's setBlockType decision: convert in the direction of
    // the linebreak replacement when the target accepts it and isn't a
    // whitespace=pre context, and the inverse direction when the target IS
    // pre and rejects the replacement node.
    if !target_is_pre && target_accepts_linebreak && has_newline_text {
        return convert_newlines_to_breaks(content, &linebreak_probe);
    }
    if target_is_pre && !target_accepts_linebreak && has_linebreak_node {
        return convert_breaks_to_newlines(schema, content, linebreak_name);
    }

    Ok(content.clone())
}

fn convert_newlines_to_breaks(
    content: &Fragment,
    linebreak_template: &Node,
) -> RichTextResult<Fragment> {
    let mut out: Vec<Node> = Vec::with_capacity(content.len());
    for child in content.iter() {
        let Some(text) = child.text() else {
            out.push(child.clone());
            continue;
        };
        if !text.contains('\n') {
            out.push(child.clone());
            continue;
        }
        let mut buffer = String::new();
        for ch in text.chars() {
            if ch == '\n' {
                if !buffer.is_empty() {
                    let mut next = child.clone();
                    next.text = Some(std::mem::take(&mut buffer));
                    out.push(next);
                }
                out.push(
                    linebreak_template
                        .clone()
                        .with_marks(child.marks().to_vec()),
                );
            } else {
                buffer.push(ch);
            }
        }
        if !buffer.is_empty() {
            let mut next = child.clone();
            next.text = Some(buffer);
            out.push(next);
        }
    }
    let mut fragment = Fragment::from(out);
    fragment.merge_adjacent_text();
    Ok(fragment)
}

fn convert_breaks_to_newlines(
    schema: &Schema,
    content: &Fragment,
    linebreak_name: &str,
) -> RichTextResult<Fragment> {
    let mut out: Vec<Node> = Vec::with_capacity(content.len());
    for child in content.iter() {
        if child.type_name() == linebreak_name {
            out.push(schema.text("\n", Vec::new())?);
        } else {
            out.push(child.clone());
        }
    }
    let mut fragment = Fragment::from(out);
    fragment.merge_adjacent_text();
    Ok(fragment)
}

fn replace_around(doc: &Node, step: &ReplaceAroundStep, schema: &Schema) -> RichTextResult<Node> {
    if step.from > step.gap_from
        || step.gap_from > step.gap_to
        || step.gap_to > step.to
        || step.to > doc.content_size()
    {
        return Err(RichTextError::InvalidPosition {
            pos: step.to,
            size: doc.content_size(),
        });
    }

    let gap = doc.slice(step.gap_from, step.gap_to)?;
    if gap.open_start != 0 || gap.open_end != 0 {
        return Err(RichTextError::Transform(
            "replace-around currently requires a closed preserved gap".to_string(),
        ));
    }

    // `step.insert` is a position in the slice's "user-facing" content (the
    // content past the open-start chain). Matching PM's Slice.insertAt, offset
    // by open_start when threading into the raw fragment so a split slice like
    // [empty_bq, empty_bq] with insert=1, openStart=1 puts the gap BETWEEN the
    // two opened wrappers instead of inside the first one.
    let content = insert_into_fragment(
        step.slice.content.clone(),
        step.insert + step.slice.open_start,
        gap.content,
    )?;
    replace(
        doc,
        step.from,
        step.to,
        Slice::new(content, step.slice.open_start, step.slice.open_end),
        schema,
    )
}

fn insert_into_fragment(
    fragment: Fragment,
    pos: usize,
    inserted: Fragment,
) -> RichTextResult<Fragment> {
    if pos > fragment.size() {
        return Err(RichTextError::InvalidPosition {
            pos,
            size: fragment.size(),
        });
    }

    let mut offset = 0;
    for (index, child) in fragment.as_slice().iter().enumerate() {
        let end = offset + child.node_size();
        if pos > offset && pos < end && !child.is_text() && !child.is_leaf() {
            // Position falls inside this child's content — recurse one
            // wrapper deeper. Matches PM's `insertInto`, which keeps
            // descending until the position lands at a fragment-level
            // boundary; only then does it splice. Without the recursion
            // a 2-wrapper insert (e.g. `bullet_list > list_item`) would
            // hit `Fragment::replace_between` and crack open the inner
            // wrapper, producing `[wrapper-open-half, gap…, wrapper-
            // close-half]` instead of the nested structure.
            let inner_pos = pos - offset - 1;
            let inner_content = insert_into_fragment(child.content().clone(), inner_pos, inserted)?;
            let mut children = fragment.as_slice().to_vec();
            let mut next = child.clone();
            next.content = inner_content;
            children[index] = next;
            return Ok(Fragment::from(children));
        }
        offset = end;
    }

    fragment.replace_between(pos, pos, inserted)
}

fn split_doc_at(doc: &Node, pos: usize, depth: usize, schema: &Schema) -> RichTextResult<Node> {
    split_doc_at_into(doc, pos, depth, &[], schema)
}

fn split_doc_at_into(
    doc: &Node,
    pos: usize,
    depth: usize,
    types_after: &[Option<TypeAfter>],
    schema: &Schema,
) -> RichTextResult<Node> {
    if depth == 0 {
        return Err(RichTextError::Transform(
            "split depth must be at least 1".to_string(),
        ));
    }

    let resolved = doc.resolve(pos)?;
    let target_depth = resolved.depth();
    if target_depth == 0 {
        return Err(RichTextError::Transform(
            "cannot split the top-level document".to_string(),
        ));
    }
    if depth > target_depth {
        return Err(RichTextError::Transform(format!(
            "split depth {depth} exceeds resolved depth {target_depth}"
        )));
    }

    let target = resolved
        .node(target_depth)
        .ok_or_else(|| RichTextError::InvalidPosition {
            pos,
            size: doc.content_size(),
        })?;
    if target.is_text() || target.is_leaf() {
        return Err(RichTextError::Transform(format!(
            "cannot split leaf node {}",
            target.type_name()
        )));
    }

    let split_offset = pos - resolved.start(target_depth).unwrap_or(pos);
    let mut left = target.copy_with_content(target.content().cut(0, split_offset)?);
    let mut right = split_right_node(
        schema,
        target,
        target.content().cut(split_offset, target.content_size())?,
        type_after_at(types_after, depth.saturating_sub(1)),
    )?;

    for level in 1..depth {
        let ancestor_depth = target_depth - level;
        let ancestor =
            resolved
                .node(ancestor_depth)
                .ok_or_else(|| RichTextError::InvalidPosition {
                    pos,
                    size: doc.content_size(),
                })?;
        let child_index =
            resolved
                .index(ancestor_depth)
                .ok_or_else(|| RichTextError::InvalidPosition {
                    pos,
                    size: doc.content_size(),
                })?;

        let mut left_children: Vec<Node> = ancestor.content().as_slice()[..child_index].to_vec();
        left_children.push(left);
        let next_left = ancestor.copy_with_content(Fragment::from(left_children));

        let mut right_children = vec![right];
        right_children.extend_from_slice(&ancestor.content().as_slice()[child_index + 1..]);
        let next_right = split_right_node(
            schema,
            ancestor,
            Fragment::from(right_children),
            type_after_at(types_after, depth - level - 1),
        )?;

        left = next_left;
        right = next_right;
    }

    let grand_depth = target_depth - depth;
    let grand_index =
        resolved
            .index(grand_depth)
            .ok_or_else(|| RichTextError::InvalidPosition {
                pos,
                size: doc.content_size(),
            })?;
    let mut new_doc = doc.clone();
    let grand_path = path_to_depth(&resolved, grand_depth)?;
    let grand = node_mut_at_path(&mut new_doc, &grand_path)?;
    grand
        .content
        .children_mut()
        .splice(grand_index..grand_index + 1, [left, right]);
    grand.content.merge_adjacent_text();
    schema.check_node(&new_doc)?;
    Ok(new_doc)
}

fn split_right_node(
    schema: &Schema,
    source: &Node,
    content: Fragment,
    type_after: Option<&TypeAfter>,
) -> RichTextResult<Node> {
    let Some(type_after) = type_after else {
        return Ok(source.copy_with_content(content));
    };
    let replacement = schema
        .node(&type_after.type_name, type_after.attrs.clone(), content)?
        .with_marks(source.marks().to_vec());
    Ok(replacement)
}

fn join_doc_at(doc: &Node, pos: usize, schema: &Schema) -> RichTextResult<Node> {
    let resolved = doc.resolve(pos)?;

    for depth in (0..=resolved.depth()).rev() {
        let Some(parent) = resolved.node(depth) else {
            continue;
        };
        let Some(index) = resolved.index(depth) else {
            continue;
        };
        if index == 0 || index >= parent.child_count() {
            continue;
        }
        if resolved.pos_at_index(index, depth) != Some(pos) {
            continue;
        }

        let left = parent.child(index - 1).expect("checked index");
        let right = parent.child(index).expect("checked index");
        if left.is_text() || right.is_text() || left.is_leaf() || right.is_leaf() {
            continue;
        }

        // Same-type join keeps left's identity. Cross-type join keeps left's type
        // but only when the schema allows left's content to absorb right's children.
        let combined = left.content().clone().append(right.content().clone());
        let left_type = schema.node_type(left.type_name())?;
        if !left_type.content_expr().matches_fragment(schema, &combined) {
            continue;
        }

        let joined = left.copy_with_content(combined);
        let mut new_doc = doc.clone();
        let parent_path = path_to_depth(&resolved, depth)?;
        let new_parent = node_mut_at_path(&mut new_doc, &parent_path)?;
        new_parent
            .content
            .children_mut()
            .splice(index - 1..index + 1, [joined]);
        new_parent.content.merge_adjacent_text();
        schema.check_node(&new_doc)?;
        return Ok(new_doc);
    }

    Err(RichTextError::Transform(format!(
        "no joinable siblings around position {pos}"
    )))
}

/// True if any node in the resolved path at a depth strictly greater than
/// `low_exclusive` and ≤ `high_inclusive` is declared isolating. Used by the
/// fitters that widen a replace range across multiple ancestors so they refuse
/// to lift content out of or merge content into an isolating wrapper.
fn path_crosses_isolating(
    resolved: &crate::model::ResolvedPos,
    low_exclusive: usize,
    high_inclusive: usize,
    schema: &Schema,
) -> RichTextResult<bool> {
    if high_inclusive <= low_exclusive {
        return Ok(false);
    }
    for depth in (low_exclusive + 1)..=high_inclusive {
        let Some(node) = resolved.node(depth) else {
            continue;
        };
        if schema.node_type(node.type_name())?.is_isolating() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Find the shallowest target depth a lift can reach without crossing an
/// isolating ancestor. Returns the depth of the deepest isolating ancestor
/// strictly above the range, or 0 if none exists.
fn max_lift_target_depth(
    from: &crate::model::ResolvedPos,
    range: &crate::model::NodeRange,
    schema: &Schema,
) -> RichTextResult<usize> {
    for depth in (0..range.depth()).rev() {
        let ancestor = from
            .node(depth)
            .ok_or_else(|| RichTextError::Transform("missing lift ancestor".to_string()))?;
        if schema.node_type(ancestor.type_name())?.is_isolating() {
            // Can't cross this boundary — the next target depth out is the
            // first valid stopping point.
            return Ok(depth + 1);
        }
    }
    Ok(0)
}

fn build_lift_around_step(
    from: &crate::model::ResolvedPos,
    to: &crate::model::ResolvedPos,
    range: &crate::model::NodeRange,
    target_depth: usize,
    schema: &Schema,
) -> RichTextResult<ReplaceAroundStep> {
    let gap_from = from
        .before(range.depth() + 1)
        .ok_or_else(|| RichTextError::Transform("missing lift gap start".to_string()))?;
    let gap_to = to
        .after(range.depth() + 1)
        .ok_or_else(|| RichTextError::Transform("missing lift gap end".to_string()))?;

    let mut start = gap_from;
    let mut end = gap_to;

    // Build the "before" wrapper chain. Each split level wraps the previous
    // chain in an empty copy of the corresponding ancestor; non-split levels
    // expand the outer start instead to consume the wrapper's opening token.
    let mut before: Vec<Node> = Vec::new();
    let mut open_start: usize = 0;
    {
        let mut splitting = false;
        let mut d = range.depth();
        while d > target_depth {
            let from_index = from.index(d).unwrap_or(0);
            if splitting || from_index > 0 {
                splitting = true;
                let ancestor = from.node(d).ok_or_else(|| {
                    RichTextError::Transform("missing lift before-ancestor".to_string())
                })?;
                let ancestor_type = schema.node_type(ancestor.type_name())?;
                let prev = std::mem::take(&mut before);
                let wrapper = Node::unchecked(
                    ancestor.type_name().to_string(),
                    ancestor.attrs().clone(),
                    Vec::new(),
                    Fragment::from(prev),
                    None,
                    ancestor_type.content_expr().is_empty(),
                );
                before = vec![wrapper];
                open_start += 1;
            } else {
                start = start.checked_sub(1).ok_or_else(|| {
                    RichTextError::Transform("lift cannot expand outer start".to_string())
                })?;
            }
            d -= 1;
        }
    }

    // Build the "after" wrapper chain symmetrically.
    let mut after: Vec<Node> = Vec::new();
    let mut open_end: usize = 0;
    {
        let mut splitting = false;
        let mut d = range.depth();
        while d > target_depth {
            let inside = to
                .after(d + 1)
                .zip(to.end(d))
                .is_some_and(|(after_pos, end_pos)| after_pos < end_pos);
            if splitting || inside {
                splitting = true;
                let ancestor = to.node(d).ok_or_else(|| {
                    RichTextError::Transform("missing lift after-ancestor".to_string())
                })?;
                let ancestor_type = schema.node_type(ancestor.type_name())?;
                let prev = std::mem::take(&mut after);
                let wrapper = Node::unchecked(
                    ancestor.type_name().to_string(),
                    ancestor.attrs().clone(),
                    Vec::new(),
                    Fragment::from(prev),
                    None,
                    ancestor_type.content_expr().is_empty(),
                );
                after = vec![wrapper];
                open_end += 1;
            } else {
                end = end.checked_add(1).ok_or_else(|| {
                    RichTextError::Transform("lift cannot expand outer end".to_string())
                })?;
            }
            d -= 1;
        }
    }

    let before_size: usize = before.iter().map(Node::node_size).sum();
    let insert = before_size.saturating_sub(open_start);

    let mut slice_children = before;
    slice_children.extend(after);
    let slice = Slice::new(Fragment::from(slice_children), open_start, open_end);

    Ok(ReplaceAroundStep {
        from: start,
        to: end,
        gap_from,
        gap_to,
        slice,
        insert,
        structure: true,
    })
}

fn lift_doc_range(doc: &Node, from: usize, to: usize, schema: &Schema) -> RichTextResult<Node> {
    if from > to {
        return Err(RichTextError::Transform(
            "lift range start must not be after end".to_string(),
        ));
    }

    let from_resolved = doc.resolve(from)?;
    let to_resolved = doc.resolve(to)?;
    let range = from_resolved
        .block_range(Some(&to_resolved))
        .ok_or_else(|| RichTextError::Transform("range cannot be lifted".to_string()))?;
    if range.depth() == 0 {
        return Err(RichTextError::Transform(
            "cannot lift top-level content".to_string(),
        ));
    }

    let start_index = range.start_index();
    let end_index = range.end_index();
    if start_index >= end_index {
        return Err(RichTextError::Transform(
            "lift range does not cover sibling nodes".to_string(),
        ));
    }

    // Walk outward from the shallowest possible lift level (range.depth() - 1)
    // toward the document root, returning the first lift that satisfies the
    // schema. This mirrors upstream's `liftTarget` walker plus its ReplaceAround
    // step: at each target_depth we re-wrap the children on either side of the
    // lift path with the original ancestor types so split-around wrappers stay
    // intact.
    let mut last_err: Option<RichTextError> = None;
    for target_depth in (0..range.depth()).rev() {
        match try_lift_to(
            doc,
            &from_resolved,
            &to_resolved,
            &range,
            target_depth,
            schema,
        ) {
            Ok(new_doc) => return Ok(new_doc),
            Err(err) => last_err = Some(err),
        }
    }

    Err(last_err.unwrap_or_else(|| {
        RichTextError::Transform("range cannot be lifted to a valid target".to_string())
    }))
}

fn try_lift_to(
    doc: &Node,
    from: &crate::model::ResolvedPos,
    to: &crate::model::ResolvedPos,
    range: &crate::model::NodeRange,
    target_depth: usize,
    schema: &Schema,
) -> RichTextResult<Node> {
    // Build the "before" and "after" wrapper chains by walking from
    // range.depth() down to target_depth+1, re-wrapping siblings outside the
    // lift path with the original ancestor types so the document structure
    // around the lifted children stays valid.
    let mut before_chain: Option<Node> = None;
    let mut after_chain: Option<Node> = None;

    for d in (target_depth + 1..=range.depth()).rev() {
        let ancestor = from
            .node(d)
            .ok_or_else(|| RichTextError::Transform("missing lift ancestor".to_string()))?;
        let start_idx = if d == range.depth() {
            range.start_index()
        } else {
            from.index(d).ok_or_else(|| {
                RichTextError::Transform("missing lift ancestor index".to_string())
            })?
        };
        let end_idx = if d == range.depth() {
            range.end_index()
        } else {
            to.index_after(d).ok_or_else(|| {
                RichTextError::Transform("missing lift ancestor end index".to_string())
            })?
        };

        let children = ancestor.content().as_slice();
        let mut before_children: Vec<Node> = children[..start_idx].to_vec();
        if let Some(prev) = before_chain.take() {
            before_children.push(prev);
        }
        if !before_children.is_empty() {
            before_chain = Some(ancestor.copy_with_content(Fragment::from(before_children)));
        }

        let mut after_children: Vec<Node> = Vec::new();
        if let Some(prev) = after_chain.take() {
            after_children.push(prev);
        }
        after_children.extend_from_slice(&children[end_idx..]);
        if !after_children.is_empty() {
            after_chain = Some(ancestor.copy_with_content(Fragment::from(after_children)));
        }
    }

    // Lifted children sit between the chains at target_depth.
    let lifted: Vec<Node> =
        range.parent().content().as_slice()[range.start_index()..range.end_index()].to_vec();

    let mut replacement: Vec<Node> = Vec::new();
    if let Some(b) = before_chain {
        replacement.push(b);
    }
    replacement.extend(lifted);
    if let Some(a) = after_chain {
        replacement.push(a);
    }

    let target_index = from
        .index(target_depth)
        .ok_or_else(|| RichTextError::Transform("missing lift target index".to_string()))?;
    let target_end_index = to
        .index_after(target_depth)
        .ok_or_else(|| RichTextError::Transform("missing lift target end index".to_string()))?;

    let mut new_doc = doc.clone();
    let target_path = path_to_depth(from, target_depth)?;
    let target_node = node_mut_at_path(&mut new_doc, &target_path)?;
    target_node
        .content
        .children_mut()
        .splice(target_index..target_end_index, replacement);
    target_node.content.merge_adjacent_text();
    schema.check_node(&new_doc)?;
    Ok(new_doc)
}

fn path_to_depth(resolved: &crate::model::ResolvedPos, depth: usize) -> RichTextResult<Vec<usize>> {
    if depth > resolved.depth() {
        return Err(RichTextError::InvalidPosition {
            pos: resolved.pos(),
            size: resolved.node(0).map(Node::content_size).unwrap_or(0),
        });
    }

    let mut path = Vec::new();
    for ancestor_depth in 0..depth {
        path.push(resolved.index(ancestor_depth).ok_or_else(|| {
            RichTextError::InvalidPosition {
                pos: resolved.pos(),
                size: resolved.node(0).map(Node::content_size).unwrap_or(0),
            }
        })?);
    }
    Ok(path)
}

fn node_mut_at_path<'a>(node: &'a mut Node, path: &[usize]) -> RichTextResult<&'a mut Node> {
    let mut current = node;
    for &index in path {
        let size = current.child_count();
        current = current
            .content
            .children_mut()
            .get_mut(index)
            .ok_or(RichTextError::InvalidPosition { pos: index, size })?;
    }
    Ok(current)
}

/// Replace content in a document.
pub fn replace(
    doc: &Node,
    from: usize,
    to: usize,
    slice: Slice,
    schema: &Schema,
) -> RichTextResult<Node> {
    if from > to || to > doc.content_size() {
        return Err(RichTextError::InvalidPosition {
            pos: to,
            size: doc.content_size(),
        });
    }

    let from = doc.resolve(from)?;
    let to = doc.resolve(to)?;
    if slice.open_start > from.depth() {
        return Err(RichTextError::Replace(
            "inserted content deeper than insertion position".to_string(),
        ));
    }
    if slice.open_end > to.depth() || from.depth() - slice.open_start != to.depth() - slice.open_end
    {
        return Err(RichTextError::Replace(
            "inconsistent open depths".to_string(),
        ));
    }

    // replace_outer (and the close_node calls inside it) already validate
    // every recursion level; an extra top-level check_node is redundant work.
    replace_outer(&from, &to, &slice, 0, schema)
}

fn replace_outer(
    from: &ResolvedPos,
    to: &ResolvedPos,
    slice: &Slice,
    depth: usize,
    schema: &Schema,
) -> RichTextResult<Node> {
    let index = from
        .index(depth)
        .ok_or_else(|| replace_error("missing start index for replace"))?;
    let node = from
        .node(depth)
        .ok_or_else(|| replace_error("missing replace node"))?;

    if Some(index) == to.index(depth) && depth < from.depth() - slice.open_start {
        let inner = replace_outer(from, to, slice, depth + 1, schema)?;
        let new_content = node
            .content()
            .replace_child(index, inner)
            .map_err(|_| replace_error("replace index outside node"))?;
        close_node(node, new_content, schema)
    } else if slice.content.is_empty() {
        close_node(node, replace_two_way(from, to, depth, schema)?, schema)
    } else if slice.open_start == 0
        && slice.open_end == 0
        && from.depth() == depth
        && to.depth() == depth
    {
        let content = node
            .content()
            .cut(0, from.parent_offset())?
            .append(slice.content.clone())
            .append(
                node.content()
                    .cut(to.parent_offset(), node.content_size())?,
            );
        close_node(node, content, schema)
    } else {
        let (start, end) = prepare_slice_for_replace(slice, from)?;
        close_node(
            node,
            replace_three_way(from, &start, &end, to, depth, schema)?,
            schema,
        )
    }
}

fn replace_three_way(
    from: &ResolvedPos,
    start: &ResolvedPos,
    end: &ResolvedPos,
    to: &ResolvedPos,
    depth: usize,
    schema: &Schema,
) -> RichTextResult<Fragment> {
    let open_start = if from.depth() > depth {
        Some(joinable_replace_node(from, start, depth + 1, schema)?)
    } else {
        None
    };
    let open_end = if to.depth() > depth {
        Some(joinable_replace_node(end, to, depth + 1, schema)?)
    } else {
        None
    };

    let mut content = Vec::new();
    add_range(None, Some(from), depth, &mut content)?;
    if let (Some(open_start), Some(open_end)) = (&open_start, &open_end) {
        if start.index(depth) == end.index(depth) {
            check_replace_join(open_start, open_end, schema)?;
            let inner = replace_three_way(from, start, end, to, depth + 1, schema)?;
            add_replace_node(close_node(open_start, inner, schema)?, &mut content);
        } else {
            add_replace_node(
                close_node(
                    open_start,
                    replace_two_way(from, start, depth + 1, schema)?,
                    schema,
                )?,
                &mut content,
            );
            add_range(Some(start), Some(end), depth, &mut content)?;
            add_replace_node(
                close_node(
                    open_end,
                    replace_two_way(end, to, depth + 1, schema)?,
                    schema,
                )?,
                &mut content,
            );
        }
    } else {
        if let Some(open_start) = open_start {
            add_replace_node(
                close_node(
                    &open_start,
                    replace_two_way(from, start, depth + 1, schema)?,
                    schema,
                )?,
                &mut content,
            );
        }
        add_range(Some(start), Some(end), depth, &mut content)?;
        if let Some(open_end) = open_end {
            add_replace_node(
                close_node(
                    &open_end,
                    replace_two_way(end, to, depth + 1, schema)?,
                    schema,
                )?,
                &mut content,
            );
        }
    }
    add_range(Some(to), None, depth, &mut content)?;
    Ok(Fragment::from(content))
}

fn replace_two_way(
    from: &ResolvedPos,
    to: &ResolvedPos,
    depth: usize,
    schema: &Schema,
) -> RichTextResult<Fragment> {
    let mut content = Vec::new();
    add_range(None, Some(from), depth, &mut content)?;
    if from.depth() > depth {
        let joined = joinable_replace_node(from, to, depth + 1, schema)?;
        let inner = replace_two_way(from, to, depth + 1, schema)?;
        add_replace_node(close_node(&joined, inner, schema)?, &mut content);
    }
    add_range(Some(to), None, depth, &mut content)?;
    Ok(Fragment::from(content))
}

fn prepare_slice_for_replace(
    slice: &Slice,
    along: &ResolvedPos,
) -> RichTextResult<(ResolvedPos, ResolvedPos)> {
    let extra = along.depth() - slice.open_start;
    let parent = along
        .node(extra)
        .ok_or_else(|| replace_error("missing slice context"))?;
    let mut node = parent.copy_with_content(slice.content.clone());
    for depth in (0..extra).rev() {
        let wrapper = along
            .node(depth)
            .ok_or_else(|| replace_error("missing slice wrapper"))?;
        node = wrapper.copy_with_content(Fragment::from(node));
    }

    let start = slice.open_start + extra;
    let end = node
        .content_size()
        .checked_sub(slice.open_end + extra)
        .ok_or_else(|| replace_error("slice open depths exceed content"))?;
    Ok((node.resolve(start)?, node.resolve(end)?))
}

fn add_range(
    start: Option<&ResolvedPos>,
    end: Option<&ResolvedPos>,
    depth: usize,
    target: &mut Vec<Node>,
) -> RichTextResult<()> {
    let node = end
        .or(start)
        .and_then(|pos| pos.node(depth))
        .ok_or_else(|| replace_error("missing range node"))?;
    let mut start_index = 0;
    let end_index = if let Some(end) = end {
        end.index(depth)
            .ok_or_else(|| replace_error("missing range end index"))?
    } else {
        node.child_count()
    };

    if let Some(start) = start {
        start_index = start
            .index(depth)
            .ok_or_else(|| replace_error("missing range start index"))?;
        if start.depth() > depth {
            start_index += 1;
        } else if start.text_offset() != 0 {
            if let Some(after) = start.node_after() {
                add_replace_node(after, target);
            }
            start_index += 1;
        }
    }

    for index in start_index..end_index {
        let child = node
            .child(index)
            .ok_or_else(|| replace_error("range child index outside node"))?;
        add_replace_node(child.clone(), target);
    }
    if let Some(end) = end
        && end.depth() == depth
        && end.text_offset() != 0
        && let Some(before) = end.node_before()
    {
        add_replace_node(before, target);
    }
    Ok(())
}

fn close_node(node: &Node, content: Fragment, schema: &Schema) -> RichTextResult<Node> {
    let closed = node.copy_with_content(content);
    if schema.check_node(&closed).is_ok() {
        return Ok(closed);
    }

    // Auto-fill the parent's content with the minimum required by its content
    // expression — but only when the existing content is tiny. The BFS in
    // `fill_before` clones the full content per state; for non-trivial content
    // the rejection is almost certainly a wrong-content error (not a
    // missing-prefix one) that prefixing can't fix, so attempting it just
    // burns a lot of allocations before timing out.
    if closed.content().len() <= 1 {
        let node_type = schema.node_type(node.type_name())?;
        if let Some(filler) = node_type
            .content_expr()
            .fill_before(schema, closed.content(), true)
        {
            let filled = node.copy_with_content(filler.append(closed.content().clone()));
            if schema.check_node(&filled).is_ok() {
                return Ok(filled);
            }
        }
    }

    schema.check_node(&closed)?;
    Ok(closed)
}

fn add_replace_node(child: Node, target: &mut Vec<Node>) {
    if let Some(last) = target.last_mut()
        && last.is_text()
        && child.is_text()
        && last.marks() == child.marks()
        && last.attrs() == child.attrs()
        && let (Some(left), Some(right)) = (&mut last.text, child.text())
    {
        left.push_str(right);
        return;
    }
    target.push(child);
}

fn check_replace_join(main: &Node, sub: &Node, schema: &Schema) -> RichTextResult<()> {
    if compatible_replace_content(main, sub, schema)? {
        Ok(())
    } else {
        Err(RichTextError::Replace(format!(
            "cannot join {} onto {}",
            sub.type_name(),
            main.type_name()
        )))
    }
}

fn joinable_replace_node(
    before: &ResolvedPos,
    after: &ResolvedPos,
    depth: usize,
    schema: &Schema,
) -> RichTextResult<Node> {
    let node = before
        .node(depth)
        .ok_or_else(|| replace_error("missing join node"))?;
    let other = after
        .node(depth)
        .ok_or_else(|| replace_error("missing join counterpart"))?;
    check_replace_join(node, other, schema)?;
    Ok(node.clone())
}

fn compatible_replace_content(left: &Node, right: &Node, schema: &Schema) -> RichTextResult<bool> {
    schema.node_type(left.type_name())?;
    schema.node_type(right.type_name())?;
    Ok(left.compatible_content(right, schema))
}

fn replace_error(message: impl Into<String>) -> RichTextError {
    RichTextError::Replace(message.into())
}

fn apply_mark(
    doc: &Node,
    from: usize,
    to: usize,
    mark: &Mark,
    add: bool,
    schema: &Schema,
) -> RichTextResult<Node> {
    if from > to || to > doc.content_size() {
        return Err(RichTextError::InvalidPosition {
            pos: to,
            size: doc.content_size(),
        });
    }

    let mut new_doc = doc.clone();
    new_doc.content = doc
        .content
        .apply_mark_between(from, to, mark, add, schema)?;
    schema.check_node(&new_doc)?;
    Ok(new_doc)
}

fn apply_node_mark(
    doc: &Node,
    pos: usize,
    mark: &Mark,
    add: bool,
    schema: &Schema,
) -> RichTextResult<Node> {
    let mut new_doc = doc.clone();
    let node = node_at_mut(&mut new_doc, pos).ok_or_else(|| RichTextError::InvalidPosition {
        pos,
        size: doc.content_size(),
    })?;
    if add {
        node.marks = schema.add_mark_to_set(&node.marks, mark.clone())?;
    } else {
        node.marks.retain(|existing| existing != mark);
    }
    schema.check_node(&new_doc)?;
    Ok(new_doc)
}

fn apply_node_attr(
    doc: &Node,
    pos: usize,
    attr: &str,
    value: Option<Value>,
    schema: &Schema,
) -> RichTextResult<Node> {
    let mut new_doc = doc.clone();
    let node = node_at_mut(&mut new_doc, pos).ok_or_else(|| RichTextError::InvalidPosition {
        pos,
        size: doc.content_size(),
    })?;
    if let Some(value) = value {
        node.attrs.insert(attr.to_string(), value);
    } else {
        node.attrs.remove(attr);
    }
    schema.check_node(&new_doc)?;
    Ok(new_doc)
}

fn slice_for_replace(doc: &Node, from: usize, to: usize) -> RichTextResult<Slice> {
    let mut offset = 0;
    for child in doc.content.iter() {
        let end = offset + child.node_size();
        if !child.is_text() && !child.is_leaf() && from > offset && to <= end.saturating_sub(1) {
            let inner_from = from - offset - 1;
            let inner_to = to - offset - 1;
            return Ok(Slice::new(child.content.cut(inner_from, inner_to)?, 0, 0));
        }
        offset = end;
    }
    doc.slice(from, to)
}

fn node_at(doc: &Node, pos: usize) -> Option<&Node> {
    let mut start = 0;
    node_at_inner(doc, pos, &mut start, true)
}

fn node_at_inner<'a>(
    node: &'a Node,
    pos: usize,
    start: &mut usize,
    root: bool,
) -> Option<&'a Node> {
    if !root && *start == pos {
        return Some(node);
    }
    if node.is_text() || node.is_leaf() {
        *start += node.node_size();
        return None;
    }

    let content_start = if root { *start } else { *start + 1 };
    let saved = *start;
    *start = content_start;
    for child in node.content.iter() {
        if let Some(found) = node_at_inner(child, pos, start, false) {
            return Some(found);
        }
    }
    *start = saved + node.node_size();
    None
}

fn node_at_mut(doc: &mut Node, pos: usize) -> Option<&mut Node> {
    node_at_mut_inner(doc, pos, 0, true)
}

fn node_at_mut_inner(node: &mut Node, pos: usize, start: usize, root: bool) -> Option<&mut Node> {
    if !root && start == pos {
        return Some(node);
    }
    if node.is_text() || node.is_leaf() {
        return None;
    }

    let mut child_start = if root { start } else { start + 1 };
    for child in node.content.children_mut().iter_mut() {
        let size = child.node_size();
        if pos >= child_start && pos < child_start + size {
            return node_at_mut_inner(child, pos, child_start, false);
        }
        child_start += size;
    }
    None
}

fn add_diff(pos: usize, diff: isize) -> usize {
    if diff < 0 {
        pos.saturating_sub(diff.unsigned_abs())
    } else {
        pos.saturating_add(diff as usize)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::schema_basic;

    fn doc() -> (Schema, Node) {
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![schema_basic::text("hello", Vec::new()).unwrap()])
                .unwrap(),
        ])
        .unwrap();
        (schema, doc)
    }

    #[test]
    fn replace_text_inside_paragraph() {
        let (schema, doc) = doc();
        let replacement = Fragment::from(schema.text("i", Vec::new()).unwrap());
        let slice = Slice::new(replacement, 0, 0);
        let mut tr = Transform::new(schema, doc);
        tr.replace(2, 5, slice).unwrap();

        assert_eq!(tr.doc().text_content(), "hio");
        assert_eq!(tr.maps()[0].map(6, 1), 4);
    }

    #[test]
    fn add_and_remove_mark_splits_text() {
        let (schema, doc) = doc();
        let strong = schema_basic::strong().unwrap();
        let mut tr = Transform::new(schema, doc);
        tr.add_mark(1, 6, strong.clone()).unwrap();
        assert_eq!(
            tr.doc()
                .content()
                .child(0)
                .unwrap()
                .content()
                .child(0)
                .unwrap()
                .marks(),
            std::slice::from_ref(&strong)
        );

        tr.remove_mark(2, 4, strong).unwrap();
        assert_eq!(tr.doc().text_content(), "hello");
        assert_eq!(tr.doc().content().child(0).unwrap().content().len(), 3);
    }

    #[test]
    fn attr_steps_update_nodes_and_doc() {
        let (schema, doc) = doc();
        let mut tr = Transform::new(schema, doc);
        tr.set_node_attr(0, "class", Some(json!("lead")))
            .unwrap()
            .set_doc_attr("version", Some(json!(1)))
            .unwrap();

        assert_eq!(
            tr.doc().content().child(0).unwrap().attrs().get("class"),
            Some(&json!("lead"))
        );
        assert_eq!(tr.doc().attrs().get("version"), Some(&json!(1)));
    }

    #[test]
    fn split_join_wrap_and_lift_blocks() {
        let schema = schema_basic::schema();
        let doc = schema_basic::doc(vec![
            schema_basic::paragraph(vec![schema_basic::text("hello", Vec::new()).unwrap()])
                .unwrap(),
        ])
        .unwrap();

        assert!(can_split(&doc, 3, 1, &schema));
        let mut tr = Transform::new(schema.clone(), doc);
        tr.split(3, 1).unwrap();
        assert_eq!(tr.doc().child_count(), 2);
        assert_eq!(
            tr.doc().content().child(0).unwrap().text_content(),
            "he".to_string()
        );
        assert_eq!(
            tr.doc().content().child(1).unwrap().text_content(),
            "llo".to_string()
        );

        tr.join(4).unwrap();
        assert_eq!(tr.doc().child_count(), 1);
        assert_eq!(tr.doc().text_content(), "hello");

        let size = tr.doc().content_size();
        tr.wrap(0, size, "blockquote", crate::model::Attrs::new())
            .unwrap();
        assert_eq!(
            tr.doc().content().child(0).unwrap().type_name(),
            "blockquote"
        );

        let blockquote_end = tr.doc().content_size() - 1;
        tr.lift(1, blockquote_end).unwrap();
        assert_eq!(
            tr.doc().content().child(0).unwrap().type_name(),
            "paragraph"
        );
        assert_eq!(tr.doc().text_content(), "hello");
    }

    #[test]
    fn replace_around_can_wrap_closed_gap() {
        let schema = Schema::builder()
            .node(crate::model::NodeSpec::new("doc").content("block+"))
            .node(
                crate::model::NodeSpec::new("paragraph")
                    .group("block")
                    .content("text*"),
            )
            .node(
                crate::model::NodeSpec::new("blockquote")
                    .group("block")
                    .content("block*"),
            )
            .node(crate::model::NodeSpec::new("text").inline())
            .finish()
            .unwrap();
        let paragraph = schema
            .node(
                "paragraph",
                Attrs::new(),
                Fragment::from(schema.text("a", Vec::new()).unwrap()),
            )
            .unwrap();
        let doc = schema
            .node("doc", Attrs::new(), Fragment::from(paragraph))
            .unwrap();
        let wrapper = schema
            .node("blockquote", Attrs::new(), Fragment::empty())
            .unwrap();
        let slice = Slice::new(Fragment::from(wrapper), 0, 0);
        let mut tr = Transform::new(schema.clone(), doc);

        tr.replace_around(0, 3, 0, 3, slice, 1).unwrap();

        assert_eq!(
            tr.doc().content().child(0).unwrap().type_name(),
            "blockquote"
        );
        assert_eq!(tr.doc().text_content(), "a");

        let inverted = tr.steps()[0].invert(&tr.docs()[0], &schema).unwrap();
        let result = inverted.apply(tr.doc(), &schema).unwrap();
        assert_eq!(result.doc, tr.docs()[0]);
    }
}
