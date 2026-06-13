use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use serde::de::{self, Deserializer};
use serde::ser::{SerializeStruct, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{RichTextError, RichTextResult};

/// Flexible attribute map used by nodes and marks.
pub type Attrs = BTreeMap<String, Value>;

/// A compiled document schema.
#[derive(Clone, Debug)]
pub struct Schema {
    inner: Arc<SchemaInner>,
}

#[derive(Debug)]
struct SchemaInner {
    nodes: BTreeMap<String, NodeType>,
    marks: BTreeMap<String, MarkType>,
    top_node: String,
    linebreak_replacement: Option<String>,
    inline_wrapper: Option<String>,
}

impl Schema {
    /// Start a new schema builder.
    pub fn builder() -> SchemaBuilder {
        SchemaBuilder::new()
    }

    /// Return the schema's top-level node type name.
    pub fn top_node_name(&self) -> &str {
        &self.inner.top_node
    }

    /// Look up a node type by name.
    pub fn node_type(&self, name: &str) -> RichTextResult<&NodeType> {
        self.inner
            .nodes
            .get(name)
            .ok_or_else(|| RichTextError::UnknownNodeType(name.to_string()))
    }

    /// Look up a mark type by name.
    pub fn mark_type(&self, name: &str) -> RichTextResult<&MarkType> {
        self.inner
            .marks
            .get(name)
            .ok_or_else(|| RichTextError::UnknownMarkType(name.to_string()))
    }

    /// Node type names in schema order.
    pub fn node_type_names(&self) -> Vec<&str> {
        let mut nodes = self.inner.nodes.values().collect::<Vec<_>>();
        nodes.sort_by_key(|node_type| node_type.rank);
        nodes.into_iter().map(NodeType::name).collect()
    }

    /// The node type marked as the schema-wide linebreak replacement, if any.
    pub fn linebreak_replacement_type(&self) -> Option<&NodeType> {
        self.inner
            .linebreak_replacement
            .as_ref()
            .and_then(|name| self.inner.nodes.get(name))
    }

    /// The first non-inline node type whose content expression accepts inline
    /// children — used by replace fitters that need to wrap inline content
    /// dropped into a block parent. Resolved once at builder time.
    pub fn inline_wrapper_type(&self) -> Option<&NodeType> {
        self.inner
            .inline_wrapper
            .as_ref()
            .and_then(|name| self.inner.nodes.get(name))
    }

    /// Produce a leaf node's synthetic text using its node-type's `leaf_text`
    /// spec hook, if one is configured. Returns `None` if the type has no
    /// custom hook or the node isn't recognized in this schema. Useful as
    /// the callback you'd pass to `Node::text_between_with`.
    pub fn leaf_text_for(&self, node: &Node) -> Option<String> {
        let node_type = self.node_type(node.type_name()).ok()?;
        let f = node_type.leaf_text()?;
        Some(f(node))
    }

    /// Build and validate a non-text node.
    pub fn node(
        &self,
        name: impl Into<String>,
        attrs: Attrs,
        content: Fragment,
    ) -> RichTextResult<Node> {
        let name = name.into();
        let node_type = self.node_type(&name)?;
        if name == "text" {
            return Err(RichTextError::InvalidContent(
                "text nodes must be constructed with Schema::text".to_string(),
            ));
        }

        let attrs = resolve_attrs(&node_type.attrs, attrs, &name)?;
        node_type.content.validate(self, &content, &name)?;
        self.validate_child_marks(node_type, &content, &name)?;
        for child in content.iter() {
            self.check_node(child)?;
        }

        Ok(Node {
            name,
            attrs,
            marks: Vec::new(),
            content,
            text: None,
            leaf: node_type.content.is_empty(),
        })
    }

    /// Build and validate a text node.
    pub fn text(&self, text: impl Into<String>, marks: Vec<Mark>) -> RichTextResult<Node> {
        self.node_type("text")?;
        let marks = self.normalize_marks(marks)?;
        Ok(Node {
            name: "text".to_string(),
            attrs: Attrs::new(),
            marks,
            content: Fragment::empty(),
            text: Some(text.into()),
            leaf: false,
        })
    }

    /// Build and validate a mark.
    pub fn mark(&self, name: impl Into<String>, attrs: Attrs) -> RichTextResult<Mark> {
        let name = name.into();
        let mark_type = self.mark_type(&name)?;
        let attrs = resolve_attrs(&mark_type.attrs, attrs, &name)?;
        Ok(Mark { name, attrs })
    }

    pub(crate) fn resolve_node_attrs(&self, name: &str, attrs: Attrs) -> RichTextResult<Attrs> {
        let node_type = self.node_type(name)?;
        resolve_attrs(&node_type.attrs, attrs, name)
    }

    /// Build the default valid node for a node type when one can be inferred.
    pub fn default_node(&self, name: &str) -> RichTextResult<Node> {
        self.default_node_with_depth(name, 0)
            .ok_or_else(|| RichTextError::InvalidContent(format!("no default node for {name}")))
    }

    /// Normalize a mark set by schema order and exclusion rules.
    pub fn normalize_marks(&self, mut marks: Vec<Mark>) -> RichTextResult<Vec<Mark>> {
        for mark in &marks {
            self.mark_type(&mark.name)?;
        }

        marks.sort_by_key(|mark| self.inner.marks[&mark.name].rank);
        let mut normalized: Vec<Mark> = Vec::new();

        for mark in marks {
            if normalized.iter().any(|existing| existing == &mark) {
                continue;
            }

            if normalized
                .iter()
                .any(|existing| self.mark_excludes(&existing.name, &mark.name))
            {
                continue;
            }

            normalized.retain(|existing| !self.mark_excludes(&mark.name, &existing.name));
            normalized.push(mark);
        }

        Ok(normalized)
    }

    /// Add a mark to an existing set using schema rank and exclusion rules.
    pub fn add_mark_to_set(&self, set: &[Mark], mark: Mark) -> RichTextResult<Vec<Mark>> {
        let mark_rank = self.mark_type(&mark.name)?.rank;
        for existing in set {
            self.mark_type(&existing.name)?;
        }

        let mut result = Vec::with_capacity(set.len() + 1);
        let mut placed = false;

        for existing in set {
            if existing == &mark {
                return Ok(set.to_vec());
            }

            if self.mark_excludes(&mark.name, &existing.name) {
                continue;
            }

            if self.mark_excludes(&existing.name, &mark.name) {
                return Ok(set.to_vec());
            }

            if !placed && self.inner.marks[&existing.name].rank > mark_rank {
                result.push(mark.clone());
                placed = true;
            }
            result.push(existing.clone());
        }

        if !placed {
            result.push(mark);
        }

        Ok(result)
    }

    /// Validate a node and all descendants against this schema.
    pub fn check_node(&self, node: &Node) -> RichTextResult<()> {
        let node_type = self.node_type(&node.name)?;
        if node.is_text() {
            if node.name != "text" {
                return Err(RichTextError::InvalidContent(format!(
                    "node {} has text content but is not text",
                    node.name
                )));
            }
            for mark in &node.marks {
                self.mark_type(&mark.name)?;
            }
            return Ok(());
        }
        if node.leaf != node_type.content.is_empty() {
            return Err(RichTextError::InvalidContent(format!(
                "node {} has stale leaf metadata",
                node.name
            )));
        }

        node_type
            .content
            .validate(self, &node.content, &node.name)?;
        self.validate_child_marks(node_type, &node.content, &node.name)?;
        for child in node.content.iter() {
            self.check_node(child)?;
        }
        Ok(())
    }

    fn validate_child_marks(
        &self,
        parent_type: &NodeType,
        content: &Fragment,
        parent: &str,
    ) -> RichTextResult<()> {
        for child in content.iter() {
            for mark in child.marks() {
                if !parent_type.marks.allows(self, mark) {
                    return Err(RichTextError::InvalidContent(format!(
                        "{parent} does not allow mark {} on child {}",
                        mark.type_name(),
                        child.type_name()
                    )));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn is_group_member(&self, node_name: &str, group: &str) -> bool {
        self.inner
            .nodes
            .get(node_name)
            .is_some_and(|node| node.groups.contains(group))
    }

    fn group_has_inline_member(&self, group: &str) -> bool {
        self.inner
            .nodes
            .values()
            .any(|node| node.inline && node.groups.contains(group))
    }

    fn node_names_for_term(&self, name: &str) -> Vec<String> {
        if self.inner.nodes.contains_key(name) {
            return vec![name.to_string()];
        }

        let mut nodes = self
            .inner
            .nodes
            .values()
            .filter(|node| node.groups.contains(name))
            .map(|node| (node.rank, node.name.clone()))
            .collect::<Vec<_>>();
        nodes.sort_by_key(|(rank, _)| *rank);
        nodes.into_iter().map(|(_, name)| name).collect()
    }

    fn default_node_with_depth(&self, name: &str, depth: usize) -> Option<Node> {
        if depth > 8 || name == "text" {
            return None;
        }

        let node_type = self.inner.nodes.get(name)?;
        let content = node_type.content.default_fragment(self, depth + 1)?;
        self.node(name, Attrs::new(), content).ok()
    }

    fn mark_excludes(&self, excluding: &str, target: &str) -> bool {
        let Some(excluding_type) = self.inner.marks.get(excluding) else {
            return false;
        };
        excluding_type.excludes_all
            || excluding_type.excludes.contains(target)
            || self.inner.marks.get(target).is_some_and(|target_type| {
                target_type
                    .groups
                    .iter()
                    .any(|group| excluding_type.excludes.contains(group))
            })
    }
}

/// Builder for [`Schema`].
#[derive(Clone, Debug)]
pub struct SchemaBuilder {
    nodes: Vec<NodeSpec>,
    marks: Vec<MarkSpec>,
    top_node: String,
}

impl Default for SchemaBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaBuilder {
    /// Create an empty schema builder.
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            marks: Vec::new(),
            top_node: "doc".to_string(),
        }
    }

    /// Set the top-level node type name.
    pub fn top_node(mut self, name: impl Into<String>) -> Self {
        self.top_node = name.into();
        self
    }

    /// Add a node spec.
    pub fn node(mut self, spec: NodeSpec) -> Self {
        self.nodes.push(spec);
        self
    }

    /// Add a mark spec.
    pub fn mark(mut self, spec: MarkSpec) -> Self {
        self.marks.push(spec);
        self
    }

    /// Compile the schema.
    pub fn finish(self) -> RichTextResult<Schema> {
        let mut nodes = BTreeMap::new();
        let mut marks = BTreeMap::new();

        for (rank, spec) in self.nodes.into_iter().enumerate() {
            if nodes.contains_key(&spec.name) {
                return Err(RichTextError::InvalidSchema(format!(
                    "duplicate node type {}",
                    spec.name
                )));
            }
            let content = ContentExpr::parse(&spec.content)?;
            nodes.insert(
                spec.name.clone(),
                NodeType {
                    name: spec.name,
                    groups: spec.groups,
                    content,
                    inline: spec.inline,
                    atom: spec.atom,
                    marks: spec.marks,
                    linebreak_replacement: spec.linebreak_replacement,
                    defining_for_content: spec.defining_for_content,
                    defining_as_context: spec.defining_as_context,
                    code: spec.code,
                    whitespace: spec.whitespace,
                    selectable: spec.selectable,
                    isolating: spec.isolating,
                    attrs: spec.attrs,
                    rank,
                    leaf_text: spec.leaf_text,
                },
            );
        }

        for (rank, spec) in self.marks.into_iter().enumerate() {
            if marks.contains_key(&spec.name) {
                return Err(RichTextError::InvalidSchema(format!(
                    "duplicate mark type {}",
                    spec.name
                )));
            }
            marks.insert(
                spec.name.clone(),
                MarkType {
                    name: spec.name,
                    groups: spec.groups,
                    excludes: spec.excludes,
                    excludes_all: spec.excludes_all,
                    inclusive: spec.inclusive,
                    attrs: spec.attrs,
                    rank,
                },
            );
        }

        if !nodes.contains_key(&self.top_node) {
            return Err(RichTextError::InvalidSchema(format!(
                "top node {} is not defined",
                self.top_node
            )));
        }

        let linebreak_replacement = nodes
            .values()
            .find(|node_type| node_type.linebreak_replacement)
            .map(|node_type| node_type.name.clone());

        // Resolve the inline-content wrapper now so fit_inline_slice... doesn't
        // re-scan node_type_names() (which allocates a Vec<String>) per call.
        // We need the cached schema to evaluate `inline_content`; build the
        // inner first without the wrapper, then peek through an Arc to compute
        // it. The wrapper-lookup itself only reads `inline_content` which is a
        // pure function of node_type + schema, so a temporary Schema suffices.
        let mut ordered: Vec<(usize, &NodeType)> = nodes.values().map(|n| (n.rank, n)).collect();
        ordered.sort_by_key(|(rank, _)| *rank);
        let probe = Schema {
            inner: Arc::new(SchemaInner {
                nodes: nodes.clone(),
                marks: marks.clone(),
                top_node: self.top_node.clone(),
                linebreak_replacement: linebreak_replacement.clone(),
                inline_wrapper: None,
            }),
        };
        let inline_wrapper = ordered
            .into_iter()
            .find(|(_, node_type)| !node_type.is_inline() && node_type.inline_content(&probe))
            .map(|(_, node_type)| node_type.name.clone());

        Ok(Schema {
            inner: Arc::new(SchemaInner {
                nodes,
                marks,
                top_node: self.top_node,
                linebreak_replacement,
                inline_wrapper,
            }),
        })
    }
}

/// Declared metadata for a single attribute on a node or mark spec.
/// Mirrors upstream's `AttributeSpec`. When no default is set the attribute
/// is required at construction time.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AttributeSpec {
    default: Option<Value>,
}

impl AttributeSpec {
    /// Construct an attribute spec with no default — the caller must supply
    /// a value when building a node or mark instance.
    pub fn required() -> Self {
        Self { default: None }
    }

    /// Construct an attribute spec with a default value.
    pub fn with_default(default: Value) -> Self {
        Self {
            default: Some(default),
        }
    }

    /// The default value, if any.
    pub fn default_value(&self) -> Option<&Value> {
        self.default.as_ref()
    }

    /// Whether this attribute has a default value (and therefore is optional).
    pub fn has_default(&self) -> bool {
        self.default.is_some()
    }
}

/// How whitespace inside a node is preserved. Matches upstream's
/// `NodeSpec.whitespace` setting and drives transforms like setBlockType's
/// newline conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Whitespace {
    /// Whitespace is collapsed (default).
    #[default]
    Normal,
    /// Whitespace (including `\n`) is preserved literally — used by code-like
    /// nodes such as `code_block` or `pre`.
    Pre,
}

/// Schema definition for one node type.
#[derive(Clone, Debug)]
pub struct NodeSpec {
    name: String,
    groups: BTreeSet<String>,
    content: String,
    inline: bool,
    atom: bool,
    marks: MarkPolicy,
    linebreak_replacement: bool,
    defining_for_content: bool,
    defining_as_context: bool,
    code: bool,
    whitespace: Whitespace,
    selectable: bool,
    isolating: bool,
    attrs: BTreeMap<String, AttributeSpec>,
    leaf_text: Option<fn(&Node) -> String>,
}

impl NodeSpec {
    /// Create a node spec with no content.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            groups: BTreeSet::new(),
            content: String::new(),
            inline: false,
            atom: false,
            marks: MarkPolicy::All,
            linebreak_replacement: false,
            defining_for_content: false,
            defining_as_context: false,
            code: false,
            whitespace: Whitespace::Normal,
            selectable: true,
            isolating: false,
            attrs: BTreeMap::new(),
            leaf_text: None,
        }
    }

    /// Provide a function that produces text for instances of this node type
    /// when extracted via `Node::text_between_with`. Mirrors PM's
    /// `NodeSpec.leafText`. Function-pointer rather than a boxed closure so
    /// the spec stays Copy-cheap.
    pub fn leaf_text(mut self, f: fn(&Node) -> String) -> Self {
        self.leaf_text = Some(f);
        self
    }

    /// Declare an optional attribute with a default value. Callers may omit
    /// it at construction time; the schema fills the default in.
    pub fn attr(mut self, name: impl Into<String>, default: Value) -> Self {
        self.attrs
            .insert(name.into(), AttributeSpec::with_default(default));
        self
    }

    /// Declare an attribute that must be provided at construction time.
    pub fn required_attr(mut self, name: impl Into<String>) -> Self {
        self.attrs.insert(name.into(), AttributeSpec::required());
        self
    }

    /// Add whitespace-separated group names.
    pub fn group(mut self, groups: impl AsRef<str>) -> Self {
        self.groups.extend(split_names(groups.as_ref()));
        self
    }

    /// Set the content expression.
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// Mark this node type as inline.
    pub fn inline(mut self) -> Self {
        self.inline = true;
        self
    }

    /// Mark this node type as an atom.
    pub fn atom(mut self) -> Self {
        self.atom = true;
        self
    }

    /// Set the mark policy for this node type.
    pub fn marks(mut self, marks: MarkPolicy) -> Self {
        self.marks = marks;
        self
    }

    /// Mark this node type as "defining for content" (matches upstream
    /// `NodeSpec.definingForContent`). Replace fitters preserve a slice's
    /// outer wrapper when its outermost defining-for-content type doesn't
    /// match the cursor's parent.
    pub fn defining_for_content(mut self) -> Self {
        self.defining_for_content = true;
        self
    }

    /// Mark this node type as providing defining context for its children
    /// (matches upstream `NodeSpec.definingAsContext`).
    pub fn defining_as_context(mut self) -> Self {
        self.defining_as_context = true;
        self
    }

    /// Convenience: turn on both defining-for-content and defining-as-context,
    /// matching upstream's `NodeSpec.defining` shortcut.
    pub fn defining(mut self) -> Self {
        self.defining_for_content = true;
        self.defining_as_context = true;
        self
    }

    /// Mark this node type as code-like (matches upstream `NodeSpec.code`).
    /// Code nodes typically pair with `whitespace(Whitespace::Pre)`.
    pub fn code(mut self) -> Self {
        self.code = true;
        self
    }

    /// Set the whitespace policy for this node type.
    pub fn whitespace(mut self, whitespace: Whitespace) -> Self {
        self.whitespace = whitespace;
        self
    }

    /// Mark this node type as non-selectable. By default a node type is
    /// selectable; matches upstream `NodeSpec.selectable: false`.
    pub fn non_selectable(mut self) -> Self {
        self.selectable = false;
        self
    }

    /// Mark this node type as isolating — replace/lift fitters won't cross
    /// its boundary. Matches upstream `NodeSpec.isolating`.
    pub fn isolating(mut self) -> Self {
        self.isolating = true;
        self
    }

    /// Mark this node type as the schema-wide linebreak replacement (matches
    /// upstream `NodeSpec.linebreakReplacement`). `setBlockType` swaps `\n`
    /// text characters and instances of this node type when converting between
    /// textblocks where one accepts newlines (code-like) and the other accepts
    /// this node.
    pub fn linebreak_replacement(mut self) -> Self {
        self.linebreak_replacement = true;
        self
    }
}

/// Schema definition for one mark type.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkSpec {
    name: String,
    groups: BTreeSet<String>,
    excludes: BTreeSet<String>,
    excludes_all: bool,
    inclusive: bool,
    attrs: BTreeMap<String, AttributeSpec>,
}

impl MarkSpec {
    /// Create a mark spec with default exclusion of same-type marks.
    pub fn new(name: impl Into<String>) -> Self {
        let name = name.into();
        let mut excludes = BTreeSet::new();
        excludes.insert(name.clone());
        Self {
            name,
            groups: BTreeSet::new(),
            excludes,
            excludes_all: false,
            inclusive: true,
            attrs: BTreeMap::new(),
        }
    }

    /// Declare an optional mark attribute with a default value.
    pub fn attr(mut self, name: impl Into<String>, default: Value) -> Self {
        self.attrs
            .insert(name.into(), AttributeSpec::with_default(default));
        self
    }

    /// Declare a mark attribute that must be provided when constructing.
    pub fn required_attr(mut self, name: impl Into<String>) -> Self {
        self.attrs.insert(name.into(), AttributeSpec::required());
        self
    }

    /// Add whitespace-separated group names.
    pub fn group(mut self, groups: impl AsRef<str>) -> Self {
        self.groups.extend(split_names(groups.as_ref()));
        self
    }

    /// Set whitespace-separated excluded mark names or `_` for all marks.
    pub fn excludes(mut self, excludes: impl AsRef<str>) -> Self {
        self.excludes.clear();
        self.excludes_all = false;
        for name in split_names(excludes.as_ref()) {
            if name == "_" {
                self.excludes_all = true;
            } else {
                self.excludes.insert(name);
            }
        }
        self
    }

    /// Set whether this mark remains active at its boundaries.
    pub fn inclusive(mut self, inclusive: bool) -> Self {
        self.inclusive = inclusive;
        self
    }
}

/// Compiled node type metadata.
#[derive(Clone, Debug)]
pub struct NodeType {
    name: String,
    groups: BTreeSet<String>,
    content: ContentExpr,
    inline: bool,
    atom: bool,
    marks: MarkPolicy,
    linebreak_replacement: bool,
    defining_for_content: bool,
    defining_as_context: bool,
    code: bool,
    whitespace: Whitespace,
    selectable: bool,
    isolating: bool,
    attrs: BTreeMap<String, AttributeSpec>,
    rank: usize,
    leaf_text: Option<fn(&Node) -> String>,
}

impl NodeType {
    /// Custom leaf-text producer registered via `NodeSpec::leaf_text`. The
    /// public entry point is `Schema::leaf_text_for(node)`; this accessor is
    /// crate-internal so callers don't end up handling raw fn-pointers.
    pub(crate) fn leaf_text(&self) -> Option<fn(&Node) -> String> {
        self.leaf_text
    }

    /// Node type name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this node is inline.
    pub fn is_inline(&self) -> bool {
        self.inline
    }

    /// Whether this node is an atom.
    pub fn is_atom(&self) -> bool {
        self.atom
    }

    /// Whether this node type can contain inline content.
    pub fn inline_content(&self, schema: &Schema) -> bool {
        self.content.allows_inline(schema)
    }

    /// Whether this node type is the schema-wide linebreak replacement.
    pub fn is_linebreak_replacement(&self) -> bool {
        self.linebreak_replacement
    }

    /// Whether this node type is defining for content (matches upstream
    /// `NodeType.definingForContent`).
    pub fn is_defining_for_content(&self) -> bool {
        self.defining_for_content
    }

    /// Whether this node type provides defining context to its children
    /// (matches upstream `NodeType.definingAsContext`).
    pub fn is_defining_as_context(&self) -> bool {
        self.defining_as_context
    }

    /// Whether this node type is code-like.
    pub fn is_code(&self) -> bool {
        self.code
    }

    /// The whitespace handling for content inside this node type.
    pub fn whitespace(&self) -> Whitespace {
        self.whitespace
    }

    /// Whether instances of this node type are selectable as node selections.
    pub fn is_selectable(&self) -> bool {
        self.selectable
    }

    /// Whether this node type prevents replace/lift transforms from crossing
    /// its boundary.
    pub fn is_isolating(&self) -> bool {
        self.isolating
    }

    /// The compiled content expression.
    pub fn content_expr(&self) -> &ContentExpr {
        &self.content
    }

    /// Whether this node type has required attributes with no defaults.
    pub fn has_required_attrs(&self) -> bool {
        self.attrs.values().any(|attr| !attr.has_default())
    }
}

impl PartialEq for NodeType {
    fn eq(&self, other: &Self) -> bool {
        // Function-pointer equality is unreliable across codegen units, so
        // leaf_text is excluded from structural equality. Destructuring forces
        // the compiler to flag any future field that's added but not compared.
        let Self {
            name,
            groups,
            content,
            inline,
            atom,
            marks,
            linebreak_replacement,
            defining_for_content,
            defining_as_context,
            code,
            whitespace,
            selectable,
            isolating,
            attrs,
            rank,
            leaf_text: _,
        } = self;
        *name == other.name
            && *groups == other.groups
            && *content == other.content
            && *inline == other.inline
            && *atom == other.atom
            && *marks == other.marks
            && *linebreak_replacement == other.linebreak_replacement
            && *defining_for_content == other.defining_for_content
            && *defining_as_context == other.defining_as_context
            && *code == other.code
            && *whitespace == other.whitespace
            && *selectable == other.selectable
            && *isolating == other.isolating
            && *attrs == other.attrs
            && *rank == other.rank
    }
}

impl PartialEq for NodeSpec {
    fn eq(&self, other: &Self) -> bool {
        let Self {
            name,
            groups,
            content,
            inline,
            atom,
            marks,
            linebreak_replacement,
            defining_for_content,
            defining_as_context,
            code,
            whitespace,
            selectable,
            isolating,
            attrs,
            leaf_text: _,
        } = self;
        *name == other.name
            && *groups == other.groups
            && *content == other.content
            && *inline == other.inline
            && *atom == other.atom
            && *marks == other.marks
            && *linebreak_replacement == other.linebreak_replacement
            && *defining_for_content == other.defining_for_content
            && *defining_as_context == other.defining_as_context
            && *code == other.code
            && *whitespace == other.whitespace
            && *selectable == other.selectable
            && *isolating == other.isolating
            && *attrs == other.attrs
    }
}

/// Compiled mark type metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct MarkType {
    name: String,
    groups: BTreeSet<String>,
    excludes: BTreeSet<String>,
    excludes_all: bool,
    inclusive: bool,
    attrs: BTreeMap<String, AttributeSpec>,
    rank: usize,
}

impl MarkType {
    /// Mark type name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether this mark excludes a given mark type.
    pub fn excludes_mark(&self, mark_name: &str) -> bool {
        self.excludes_all || self.excludes.contains(mark_name)
    }

    /// Whether this mark remains active at its boundaries.
    pub fn is_inclusive(&self) -> bool {
        self.inclusive
    }
}

/// Mark policy for a node type.
#[derive(Clone, Debug, PartialEq)]
pub enum MarkPolicy {
    /// Any mark in the schema is allowed.
    All,
    /// No marks are allowed.
    None,
    /// Only named marks or mark groups are allowed.
    Set(BTreeSet<String>),
}

impl MarkPolicy {
    /// Build a mark policy from ProseMirror-style mark expression text.
    pub fn parse(expr: &str) -> Self {
        let expr = expr.trim();
        if expr == "_" {
            Self::All
        } else if expr.is_empty() {
            Self::None
        } else {
            Self::Set(split_names(expr).collect())
        }
    }

    fn allows(&self, schema: &Schema, mark: &Mark) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Set(names) => {
                names.contains(&mark.name)
                    || schema.inner.marks.get(&mark.name).is_some_and(|mark_type| {
                        mark_type.groups.iter().any(|group| names.contains(group))
                    })
            }
        }
    }
}

/// A compiled content expression.
#[derive(Clone, Debug, PartialEq)]
pub struct ContentExpr {
    root: ContentNode,
}

impl ContentExpr {
    /// Parse a ProseMirror-style content expression subset.
    pub fn parse(expr: &str) -> RichTextResult<Self> {
        let expr = expr.trim();
        Ok(Self {
            root: ContentParser::new(expr).parse()?,
        })
    }

    /// Validate a fragment against this expression.
    pub fn validate(
        &self,
        schema: &Schema,
        content: &Fragment,
        parent: &str,
    ) -> RichTextResult<()> {
        let matches = self.root.match_at(schema, content.as_slice(), 0);
        if matches.contains(&content.len()) {
            Ok(())
        } else {
            let child_index = matches.into_iter().max().unwrap_or(0).min(content.len());
            let child = content.children.get(child_index);
            let child_name = child.map(Node::type_name).unwrap_or("<end>");
            Err(RichTextError::InvalidContent(format!(
                "{parent} cannot contain {child_name} at child index {child_index}"
            )))
        }
    }

    /// Return true when a fragment fully matches this expression.
    pub fn matches_fragment(&self, schema: &Schema, content: &Fragment) -> bool {
        self.root
            .match_at(schema, content.as_slice(), 0)
            .contains(&content.len())
    }

    /// Match a fragment as a possible prefix of this expression.
    pub fn match_fragment<'a>(
        &'a self,
        schema: &'a Schema,
        content: &Fragment,
    ) -> Option<ContentMatch<'a>> {
        if self.can_match_prefix(schema, content) {
            Some(ContentMatch {
                expr: self,
                schema,
                matched: content.clone(),
            })
        } else {
            None
        }
    }

    /// Find minimal content that can be inserted before `after`.
    pub fn fill_before(&self, schema: &Schema, after: &Fragment, to_end: bool) -> Option<Fragment> {
        self.fill_between(schema, &Fragment::empty(), after, to_end)
    }

    /// Return true when this expression allows no content.
    pub fn is_empty(&self) -> bool {
        matches!(self.root, ContentNode::Empty)
    }

    /// Return true when this expression can match inline child nodes.
    pub fn allows_inline(&self, schema: &Schema) -> bool {
        self.root.allows_inline(schema)
    }

    fn can_match_prefix(&self, schema: &Schema, content: &Fragment) -> bool {
        !self
            .root
            .residuals_after(schema, content.as_slice())
            .is_empty()
    }

    fn fill_between(
        &self,
        schema: &Schema,
        before: &Fragment,
        after: &Fragment,
        to_end: bool,
    ) -> Option<Fragment> {
        const MAX_FILL_DEPTH: usize = 16;
        const MAX_FILL_STATES: usize = 4096;

        let candidates = self.default_fill_candidates(schema);
        let mut queue = VecDeque::from([Vec::<Node>::new()]);

        while let Some(fill) = queue.pop_front() {
            let fill_fragment = Fragment::from(fill.clone());
            let content = before
                .clone()
                .append(fill_fragment.clone())
                .append(after.clone());
            let matches = if to_end {
                self.matches_fragment(schema, &content)
            } else {
                self.can_complete(schema, &content)
            };
            if matches {
                return Some(fill_fragment);
            }

            if fill.len() >= MAX_FILL_DEPTH {
                continue;
            }

            for candidate in &candidates {
                if queue.len() >= MAX_FILL_STATES {
                    break;
                }
                let mut next = fill.clone();
                next.push(candidate.clone());
                queue.push_back(next);
            }
        }

        None
    }

    fn can_complete(&self, schema: &Schema, content: &Fragment) -> bool {
        const MAX_COMPLETION_DEPTH: usize = 16;
        const MAX_COMPLETION_STATES: usize = 4096;

        if self.matches_fragment(schema, content) {
            return true;
        }

        let candidates = self.default_fill_candidates(schema);
        let mut queue = VecDeque::from([content.clone()]);
        for _ in 0..MAX_COMPLETION_DEPTH {
            let level_count = queue.len();
            for _ in 0..level_count {
                let current = queue.pop_front().expect("level count came from queue");
                for candidate in &candidates {
                    let next = current.clone().append(Fragment::from(candidate.clone()));
                    if self.matches_fragment(schema, &next) {
                        return true;
                    }
                    if queue.len() < MAX_COMPLETION_STATES {
                        queue.push_back(next);
                    }
                }
            }
        }

        false
    }

    fn default_fill_candidates(&self, schema: &Schema) -> Vec<Node> {
        let mut names = BTreeSet::new();
        self.root.collect_terms(&mut names);

        let mut nodes = names
            .into_iter()
            .flat_map(|name| schema.node_names_for_term(&name))
            .filter_map(|name| {
                let rank = schema.inner.nodes.get(&name)?.rank;
                let node = schema.default_node_with_depth(&name, 0)?;
                Some((rank, node))
            })
            .collect::<Vec<_>>();
        nodes.sort_by_key(|(rank, _)| *rank);
        nodes.into_iter().map(|(_, node)| node).collect()
    }

    fn default_fragment(&self, schema: &Schema, depth: usize) -> Option<Fragment> {
        self.root.default_fragment(schema, depth)
    }
}

/// A cursor into a content expression after matching a fragment prefix.
#[derive(Clone, Debug)]
pub struct ContentMatch<'a> {
    expr: &'a ContentExpr,
    schema: &'a Schema,
    matched: Fragment,
}

impl<'a> ContentMatch<'a> {
    /// Fragment that was matched to produce this cursor.
    pub fn matched(&self) -> &Fragment {
        &self.matched
    }

    /// Whether the matched prefix is already a valid end.
    pub fn valid_end(&self) -> bool {
        self.expr.matches_fragment(self.schema, &self.matched)
    }

    /// Match another fragment after this cursor.
    pub fn match_fragment(&self, content: &Fragment) -> Option<Self> {
        let combined = self.matched.clone().append(content.clone());
        self.expr.match_fragment(self.schema, &combined)
    }

    /// Find minimal content that can be inserted before `after`.
    pub fn fill_before(&self, after: &Fragment, to_end: bool) -> Option<Fragment> {
        self.expr
            .fill_between(self.schema, &self.matched, after, to_end)
    }

    /// Match a single node of the given type, returning the new cursor.
    /// Mirrors upstream `ContentMatch.matchType`: only the node type is
    /// consulted, not a fully materialized default node. This keeps invalid
    /// candidates cheap in schemas where `createAndFill` would recurse through
    /// required wrappers.
    pub fn match_type(&self, node_type: &NodeType) -> Option<Self> {
        let synthetic = synthetic_node_for_type(node_type);
        self.match_fragment(&Fragment::from(synthetic))
    }

    /// The first outgoing non-text node type that can be generated without
    /// required attributes. Mirrors upstream `ContentMatch.defaultType`.
    pub fn default_type(&self) -> Option<&'a NodeType> {
        self.next_node_type_names().into_iter().find_map(|name| {
            let node_type = self.schema.node_type(&name).ok()?;
            if node_type.name() != "text" && !node_type.has_required_attrs() {
                Some(node_type)
            } else {
                None
            }
        })
    }

    /// The first default type that is a textblock.
    pub fn default_textblock_type(&self) -> Option<&'a NodeType> {
        self.next_node_type_names().into_iter().find_map(|name| {
            let node_type = self.schema.node_type(&name).ok()?;
            if node_type.name() != "text"
                && !node_type.is_inline()
                && node_type.inline_content(self.schema)
                && !node_type.has_required_attrs()
            {
                Some(node_type)
            } else {
                None
            }
        })
    }

    fn next_node_type_names(&self) -> Vec<String> {
        let mut ranked = self
            .expr
            .root
            .residuals_after(self.schema, self.matched.as_slice())
            .into_iter()
            .flat_map(|residual| residual.first_terms())
            .flat_map(|term| self.schema.node_names_for_term(&term))
            .filter_map(|name| {
                let rank = self.schema.inner.nodes.get(&name)?.rank;
                Some((rank, name))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));

        let mut seen = BTreeSet::new();
        ranked
            .into_iter()
            .filter_map(|(_, name)| {
                if seen.insert(name.clone()) {
                    Some(name)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Find a chain of wrapper node types whose nesting around `target` produces
    /// content valid at this cursor. Returns `None` if no chain (including the
    /// empty one — meaning `target` fits directly) makes the content valid.
    ///
    /// Mirrors upstream `ContentMatch.findWrapping(target)`. Pine implements
    /// this as a BFS through node types reachable from this cursor: it tries
    /// the empty chain first, then chains of length 1, 2, … up to a depth bound.
    /// Each candidate must satisfy two checks:
    ///   1. The outermost wrapper's type fits at this cursor (per `match_type`).
    ///   2. Every wrapper's content match accepts the next wrapper, and the
    ///      innermost wrapper's content match accepts a `target` instance.
    pub fn find_wrapping(&self, target: &NodeType) -> Option<Vec<NodeType>> {
        // Empty chain: target fits directly.
        if self.match_type(target).is_some() {
            return Some(Vec::new());
        }

        // BFS through (outer_type, chain_so_far). chain_so_far is the list of
        // outer wrappers; the innermost wrapper isn't included yet because we
        // need to know whether the LAST wrapper's content match accepts target.
        let mut queue: std::collections::VecDeque<(NodeType, Vec<NodeType>)> =
            std::collections::VecDeque::new();
        let mut visited = std::collections::BTreeSet::<String>::new();

        for (name, _) in self.schema.inner.nodes.iter() {
            let candidate = match self.schema.node_type(name) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if visited.insert(candidate.name().to_string()) && self.match_type(candidate).is_some()
            {
                queue.push_back((candidate.clone(), Vec::new()));
            }
        }

        const MAX_WRAP_DEPTH: usize = 6;
        while let Some((current, chain)) = queue.pop_front() {
            if chain.len() >= MAX_WRAP_DEPTH {
                continue;
            }
            let inner_match = current
                .content_expr()
                .match_fragment(self.schema, &Fragment::empty());
            let Some(inner_match) = inner_match else {
                continue;
            };
            // Does target fit inside `current` directly? If so this chain is a win.
            if inner_match.match_type(target).is_some() {
                let mut result = chain;
                result.push(current);
                return Some(result);
            }
            // Otherwise enumerate every node type that fits inside `current` and
            // continue the search.
            for (name, _) in self.schema.inner.nodes.iter() {
                let next = match self.schema.node_type(name) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if inner_match.match_type(next).is_none() {
                    continue;
                }
                let mut key = current.name().to_string();
                key.push('/');
                key.push_str(next.name());
                if !visited.insert(key) {
                    continue;
                }
                let mut next_chain = chain.clone();
                next_chain.push(current.clone());
                queue.push_back((next.clone(), next_chain));
            }
        }

        None
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ContentNode {
    Empty,
    Name(String),
    Seq(Vec<ContentNode>),
    Choice(Vec<ContentNode>),
    Repeat {
        node: Box<ContentNode>,
        min: usize,
        max: Option<usize>,
    },
}

impl ContentNode {
    fn nullable(&self) -> bool {
        match self {
            Self::Empty => true,
            Self::Name(_) => false,
            Self::Seq(parts) => parts.iter().all(Self::nullable),
            Self::Choice(parts) => parts.iter().any(Self::nullable),
            Self::Repeat { node, min, .. } => *min == 0 || node.nullable(),
        }
    }

    fn first_terms(&self) -> Vec<String> {
        match self {
            Self::Empty => Vec::new(),
            Self::Name(name) => vec![name.clone()],
            Self::Seq(parts) => {
                let mut terms = Vec::new();
                for part in parts {
                    terms.extend(part.first_terms());
                    if !part.nullable() {
                        break;
                    }
                }
                terms
            }
            Self::Choice(parts) => parts.iter().flat_map(Self::first_terms).collect(),
            Self::Repeat { node, .. } => node.first_terms(),
        }
    }

    fn residuals_after(&self, schema: &Schema, children: &[Node]) -> Vec<ContentNode> {
        let mut residuals = vec![self.clone()];
        for child in children {
            let mut next = Vec::new();
            for residual in residuals {
                push_unique_nodes(&mut next, residual.consume_child(schema, child));
            }
            if next.is_empty() {
                return Vec::new();
            }
            residuals = next;
        }
        residuals
    }

    fn consume_child(&self, schema: &Schema, child: &Node) -> Vec<ContentNode> {
        match self {
            Self::Empty => Vec::new(),
            Self::Name(name) => {
                if child.name == *name || schema.is_group_member(&child.name, name) {
                    vec![Self::Empty]
                } else {
                    Vec::new()
                }
            }
            Self::Seq(parts) => consume_child_in_sequence(parts, schema, child),
            Self::Choice(parts) => {
                let mut out = Vec::new();
                for part in parts {
                    push_unique_nodes(&mut out, part.consume_child(schema, child));
                }
                out
            }
            Self::Repeat { node, min, max } => {
                if matches!(max, Some(0)) {
                    return Vec::new();
                }
                let mut out = Vec::new();
                let rest = repeat_node(
                    node.as_ref().clone(),
                    min.saturating_sub(1),
                    max.map(|max| max.saturating_sub(1)),
                );
                for residual in node.consume_child(schema, child) {
                    out.push(seq_node(vec![residual, rest.clone()]));
                }
                out
            }
        }
    }

    fn collect_terms(&self, names: &mut BTreeSet<String>) {
        match self {
            Self::Empty => {}
            Self::Name(name) => {
                names.insert(name.clone());
            }
            Self::Seq(parts) | Self::Choice(parts) => {
                for part in parts {
                    part.collect_terms(names);
                }
            }
            Self::Repeat { node, .. } => node.collect_terms(names),
        }
    }

    fn default_fragment(&self, schema: &Schema, depth: usize) -> Option<Fragment> {
        if depth > 8 {
            return None;
        }

        match self {
            Self::Empty => Some(Fragment::empty()),
            Self::Name(name) => schema
                .node_names_for_term(name)
                .into_iter()
                .find_map(|name| schema.default_node_with_depth(&name, depth + 1))
                .map(Fragment::from),
            Self::Seq(parts) => {
                let mut fragment = Fragment::empty();
                for part in parts {
                    fragment = fragment.append(part.default_fragment(schema, depth + 1)?);
                }
                Some(fragment)
            }
            Self::Choice(parts) => parts
                .iter()
                .find_map(|part| part.default_fragment(schema, depth + 1)),
            Self::Repeat { node, min, .. } => {
                let mut fragment = Fragment::empty();
                for _ in 0..*min {
                    fragment = fragment.append(node.default_fragment(schema, depth + 1)?);
                }
                Some(fragment)
            }
        }
    }

    fn allows_inline(&self, schema: &Schema) -> bool {
        match self {
            Self::Empty => false,
            Self::Name(name) => {
                schema.node_type(name).is_ok_and(NodeType::is_inline)
                    || schema.group_has_inline_member(name)
            }
            Self::Seq(parts) | Self::Choice(parts) => {
                parts.iter().any(|part| part.allows_inline(schema))
            }
            Self::Repeat { node, .. } => node.allows_inline(schema),
        }
    }

    fn match_at(&self, schema: &Schema, children: &[Node], index: usize) -> Vec<usize> {
        match self {
            Self::Empty => vec![index],
            Self::Name(name) => {
                if children.get(index).is_some_and(|child| {
                    child.name == *name || schema.is_group_member(&child.name, name)
                }) {
                    vec![index + 1]
                } else {
                    Vec::new()
                }
            }
            Self::Seq(parts) => {
                let mut positions = vec![index];
                for part in parts {
                    let mut next_positions = BTreeSet::new();
                    for pos in positions {
                        next_positions.extend(part.match_at(schema, children, pos));
                    }
                    if next_positions.is_empty() {
                        return Vec::new();
                    }
                    positions = next_positions.into_iter().collect();
                }
                positions
            }
            Self::Choice(choices) => {
                let mut positions = BTreeSet::new();
                for choice in choices {
                    positions.extend(choice.match_at(schema, children, index));
                }
                positions.into_iter().collect()
            }
            Self::Repeat { node, min, max } => {
                let mut results = BTreeSet::new();
                let mut frontier = BTreeSet::from([index]);
                if *min == 0 {
                    results.insert(index);
                }

                let mut count = 0;
                while max.is_none_or(|max| count < max) {
                    let mut next_frontier = BTreeSet::new();
                    for pos in &frontier {
                        for next in node.match_at(schema, children, *pos) {
                            if next != *pos {
                                next_frontier.insert(next);
                            }
                        }
                    }
                    if next_frontier.is_empty() {
                        break;
                    }
                    count += 1;
                    if count >= *min {
                        results.extend(next_frontier.iter().copied());
                    }
                    frontier = next_frontier;
                }

                results.into_iter().collect()
            }
        }
    }
}

fn consume_child_in_sequence(
    parts: &[ContentNode],
    schema: &Schema,
    child: &Node,
) -> Vec<ContentNode> {
    let mut out = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if !parts[..index].iter().all(ContentNode::nullable) {
            break;
        }
        let suffix = parts[index + 1..].to_vec();
        for residual in part.consume_child(schema, child) {
            let mut next = Vec::with_capacity(1 + suffix.len());
            next.push(residual);
            next.extend(suffix.clone());
            out.push(seq_node(next));
        }
    }
    out
}

fn push_unique_nodes(target: &mut Vec<ContentNode>, nodes: Vec<ContentNode>) {
    for node in nodes {
        if !target.contains(&node) {
            target.push(node);
        }
    }
}

fn seq_node(parts: Vec<ContentNode>) -> ContentNode {
    let mut normalized = parts
        .into_iter()
        .filter(|part| !matches!(part, ContentNode::Empty))
        .collect::<Vec<_>>();
    match normalized.len() {
        0 => ContentNode::Empty,
        1 => normalized.pop().expect("one normalized node"),
        _ => ContentNode::Seq(normalized),
    }
}

fn repeat_node(node: ContentNode, min: usize, max: Option<usize>) -> ContentNode {
    if matches!(max, Some(0)) || matches!(node, ContentNode::Empty) {
        ContentNode::Empty
    } else {
        ContentNode::Repeat {
            node: Box::new(node),
            min,
            max,
        }
    }
}

fn synthetic_node_for_type(node_type: &NodeType) -> Node {
    Node {
        name: node_type.name().to_string(),
        attrs: Attrs::new(),
        marks: Vec::new(),
        content: Fragment::empty(),
        text: (node_type.name() == "text").then(String::new),
        leaf: node_type.content_expr().is_empty(),
    }
}

struct ContentParser<'a> {
    input: &'a str,
    chars: Vec<char>,
    pos: usize,
}

impl<'a> ContentParser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            chars: input.chars().collect(),
            pos: 0,
        }
    }

    fn parse(mut self) -> RichTextResult<ContentNode> {
        let node = self.parse_choice()?;
        self.skip_ws();
        if self.peek().is_some() {
            return Err(RichTextError::InvalidSchema(format!(
                "unexpected trailing content in expression {:?}",
                self.input
            )));
        }
        Ok(node)
    }

    fn parse_choice(&mut self) -> RichTextResult<ContentNode> {
        let mut choices = vec![self.parse_sequence()?];
        loop {
            self.skip_ws();
            if self.peek() != Some('|') {
                break;
            }
            self.pos += 1;
            choices.push(self.parse_sequence()?);
        }
        if choices.len() == 1 {
            Ok(choices.pop().expect("one choice"))
        } else {
            Ok(ContentNode::Choice(choices))
        }
    }

    fn parse_sequence(&mut self) -> RichTextResult<ContentNode> {
        let mut parts = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None | Some(')') | Some('|') => break,
                _ => parts.push(self.parse_repeat()?),
            }
        }
        match parts.len() {
            0 => Ok(ContentNode::Empty),
            1 => Ok(parts.pop().expect("one sequence item")),
            _ => Ok(ContentNode::Seq(parts)),
        }
    }

    fn parse_repeat(&mut self) -> RichTextResult<ContentNode> {
        let mut node = self.parse_atom()?;
        self.skip_ws();
        let repeat = match self.peek() {
            Some('?') => {
                self.pos += 1;
                Some((0, Some(1)))
            }
            Some('*') => {
                self.pos += 1;
                Some((0, None))
            }
            Some('+') => {
                self.pos += 1;
                Some((1, None))
            }
            Some('{') => Some(self.parse_count()?),
            _ => None,
        };

        if let Some((min, max)) = repeat {
            node = ContentNode::Repeat {
                node: Box::new(node),
                min,
                max,
            };
        }
        Ok(node)
    }

    fn parse_atom(&mut self) -> RichTextResult<ContentNode> {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let node = self.parse_choice()?;
                self.skip_ws();
                if self.peek() != Some(')') {
                    return Err(RichTextError::InvalidSchema(format!(
                        "unclosed group in content expression {:?}",
                        self.input
                    )));
                }
                self.pos += 1;
                Ok(node)
            }
            Some(ch) if is_name_char(ch) => self.parse_name().map(ContentNode::Name),
            _ => Err(RichTextError::InvalidSchema(format!(
                "expected content expression atom in {:?}",
                self.input
            ))),
        }
    }

    fn parse_name(&mut self) -> RichTextResult<String> {
        let start = self.pos;
        while self.peek().is_some_and(is_name_char) {
            self.pos += 1;
        }
        let name: String = self.chars[start..self.pos].iter().collect();
        if name.is_empty() {
            Err(RichTextError::InvalidSchema(format!(
                "expected name in content expression {:?}",
                self.input
            )))
        } else {
            Ok(name)
        }
    }

    fn parse_count(&mut self) -> RichTextResult<(usize, Option<usize>)> {
        self.expect('{')?;
        self.skip_ws();
        let min = self.parse_number()?;
        self.skip_ws();
        let max = if self.peek() == Some(',') {
            self.pos += 1;
            self.skip_ws();
            if self.peek() == Some('}') {
                None
            } else {
                Some(self.parse_number()?)
            }
        } else {
            Some(min)
        };
        self.skip_ws();
        self.expect('}')?;
        if max.is_some_and(|max| max < min) {
            return Err(RichTextError::InvalidSchema(format!(
                "invalid counted range in {:?}",
                self.input
            )));
        }
        Ok((min, max))
    }

    fn parse_number(&mut self) -> RichTextResult<usize> {
        let start = self.pos;
        while self.peek().is_some_and(|ch| ch.is_ascii_digit()) {
            self.pos += 1;
        }
        let raw: String = self.chars[start..self.pos].iter().collect();
        if raw.is_empty() {
            return Err(RichTextError::InvalidSchema(format!(
                "expected count in content expression {:?}",
                self.input
            )));
        }
        raw.parse::<usize>().map_err(|_| {
            RichTextError::InvalidSchema(format!("invalid count {raw:?} in {:?}", self.input))
        })
    }

    fn expect(&mut self, ch: char) -> RichTextResult<()> {
        if self.peek() == Some(ch) {
            self.pos += 1;
            Ok(())
        } else {
            Err(RichTextError::InvalidSchema(format!(
                "expected {ch:?} in content expression {:?}",
                self.input
            )))
        }
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
}

/// A mark instance.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    name: String,
    attrs: Attrs,
}

impl Mark {
    /// Create an unchecked mark. Prefer [`Schema::mark`] when validating input.
    pub fn new(name: impl Into<String>, attrs: Attrs) -> Self {
        Self {
            name: name.into(),
            attrs,
        }
    }

    /// Mark type name.
    pub fn type_name(&self) -> &str {
        &self.name
    }

    /// Mark attributes.
    pub fn attrs(&self) -> &Attrs {
        &self.attrs
    }

    /// Whether an identical mark exists in a set.
    pub fn is_in_set(&self, set: &[Mark]) -> bool {
        set.iter().any(|mark| mark == self)
    }

    /// Add this mark to a set using schema rank and exclusion rules.
    pub fn add_to_set(&self, schema: &Schema, set: &[Mark]) -> RichTextResult<Vec<Mark>> {
        schema.add_mark_to_set(set, self.clone())
    }

    /// Remove identical marks from a set.
    pub fn remove_from_set(&self, set: &[Mark]) -> Vec<Mark> {
        set.iter().filter(|mark| *mark != self).cloned().collect()
    }

    /// Test whether two mark sets are identical and in the same order.
    pub fn same_set(left: &[Mark], right: &[Mark]) -> bool {
        left == right
    }
}

impl Serialize for Mark {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("Mark", 2)?;
        state.serialize_field("type", &self.name)?;
        if !self.attrs.is_empty() {
            state.serialize_field("attrs", &self.attrs)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for Mark {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct MarkJson {
            #[serde(rename = "type")]
            name: String,
            #[serde(default)]
            attrs: Attrs,
        }

        let value = MarkJson::deserialize(deserializer)?;
        Ok(Self {
            name: value.name,
            attrs: value.attrs,
        })
    }
}

/// A persistent document node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub(crate) name: String,
    pub(crate) attrs: Attrs,
    pub(crate) marks: Vec<Mark>,
    pub(crate) content: Fragment,
    pub(crate) text: Option<String>,
    leaf: bool,
}

/// A direct child lookup result.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChildRef<'a> {
    /// Child node, if a child exists at the requested side.
    pub node: Option<&'a Node>,
    /// Child index in the parent content.
    pub index: usize,
    /// Child start offset relative to the parent content.
    pub offset: usize,
}

impl Node {
    /// Construct a node WITHOUT validating its content against the schema.
    ///
    /// Bypasses every check that `Schema::node` runs: content expression,
    /// required attributes, mark allowlist, leaf flag. Used internally when a
    /// node is intentionally a partial intermediate (open slice wrappers,
    /// lift/wrap empties), and exposed for callers that need to build the same
    /// kind of intermediate. If you call this for content destined to land in
    /// a real document, run it through `Schema::check_node` before applying.
    pub fn unchecked(
        name: String,
        attrs: Attrs,
        marks: Vec<Mark>,
        content: Fragment,
        text: Option<String>,
        leaf: bool,
    ) -> Self {
        Self {
            name,
            attrs,
            marks,
            content,
            text,
            leaf,
        }
    }

    /// Node type name.
    pub fn type_name(&self) -> &str {
        &self.name
    }

    /// Node attributes.
    pub fn attrs(&self) -> &Attrs {
        &self.attrs
    }

    /// Node marks.
    pub fn marks(&self) -> &[Mark] {
        &self.marks
    }

    /// Create a copy of this node with a different mark set.
    pub fn with_marks(&self, marks: Vec<Mark>) -> Self {
        let mut node = self.clone();
        node.marks = marks;
        node
    }

    /// Child content.
    pub fn content(&self) -> &Fragment {
        &self.content
    }

    /// Text content for text nodes.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Whether this node is a text node.
    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    /// Whether this node has no child content.
    pub fn is_leaf(&self) -> bool {
        self.leaf
    }

    /// Number of child nodes.
    pub fn child_count(&self) -> usize {
        self.content.len()
    }

    /// Get a child node.
    pub fn child(&self, index: usize) -> Option<&Node> {
        self.content.child(index)
    }

    /// Find the descendant node directly after a content position.
    pub fn node_at(&self, pos: usize) -> RichTextResult<Option<&Node>> {
        if pos > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos,
                size: self.content_size(),
            });
        }

        let mut node = self;
        let mut inner_pos = pos;
        loop {
            let (index, offset) = node.content.find_index(inner_pos);
            let Some(child) = node.child(index) else {
                return Ok(None);
            };
            if offset == inner_pos || child.is_text() {
                return Ok(Some(child));
            }
            if child.is_leaf() {
                return Ok(None);
            }
            inner_pos = inner_pos.saturating_sub(offset + 1);
            node = child;
        }
    }

    /// Find the direct child after a content position.
    pub fn child_after(&self, pos: usize) -> RichTextResult<ChildRef<'_>> {
        if pos > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos,
                size: self.content_size(),
            });
        }

        let (index, offset) = self.content.find_index(pos);
        Ok(ChildRef {
            node: self.child(index),
            index,
            offset,
        })
    }

    /// Find the direct child before a content position.
    pub fn child_before(&self, pos: usize) -> RichTextResult<ChildRef<'_>> {
        if pos > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos,
                size: self.content_size(),
            });
        }
        if pos == 0 {
            return Ok(ChildRef {
                node: None,
                index: 0,
                offset: 0,
            });
        }

        let (index, offset) = self.content.find_index(pos);
        if offset < pos {
            Ok(ChildRef {
                node: self.child(index),
                index,
                offset,
            })
        } else {
            let before_index = index - 1;
            let node = self
                .child(before_index)
                .expect("position after zero has a previous child");
            Ok(ChildRef {
                node: Some(node),
                index: before_index,
                offset: offset - node.node_size(),
            })
        }
    }

    /// Size of the node's content.
    pub fn content_size(&self) -> usize {
        if let Some(text) = &self.text {
            text_len(text)
        } else {
            self.content.size()
        }
    }

    /// ProseMirror-style node size.
    pub fn node_size(&self) -> usize {
        if self.is_text() {
            self.content_size()
        } else if self.leaf {
            1
        } else {
            self.content.size() + 2
        }
    }

    /// Concatenate descendant text.
    pub fn text_content(&self) -> String {
        if let Some(text) = &self.text {
            text.clone()
        } else {
            self.content.text_between(0, self.content.size(), "")
        }
    }

    /// Concatenate descendant text in a content-position range.
    pub fn text_between(
        &self,
        from: usize,
        to: usize,
        block_separator: &str,
    ) -> RichTextResult<String> {
        self.text_between_with(from, to, block_separator, None::<&dyn Fn(&Node) -> String>)
    }

    /// Like `text_between`, but with optional leaf-node text production. When
    /// `leaf_text` is `Some(f)`, every leaf node passed through the range has
    /// `f(node)` called to produce its synthetic text. When `None`, leaf nodes
    /// contribute nothing (matching PM's `textBetween` default with no
    /// leaf-text argument).
    ///
    /// Mirrors upstream `Node.textBetween(from, to, blockSeparator, leafText)`
    /// — the callback takes precedence over any `NodeSpec.leaf_text` hook
    /// (see `Schema::leaf_text_for`).
    pub fn text_between_with(
        &self,
        from: usize,
        to: usize,
        block_separator: &str,
        leaf_text: Option<&dyn Fn(&Node) -> String>,
    ) -> RichTextResult<String> {
        if from > to || to > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.content_size(),
            });
        }
        if let Some(text) = &self.text {
            return Ok(slice_text(text, from, to));
        }
        Ok(self
            .content
            .text_between_with(from, to, block_separator, leaf_text))
    }

    /// Resolve a document content position.
    pub fn resolve(&self, pos: usize) -> RichTextResult<ResolvedPos> {
        if pos > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos,
                size: self.content_size(),
            });
        }

        let mut path = Vec::new();
        let mut node = self.clone();
        let mut start = 0;
        let mut parent_offset = pos;

        loop {
            let (index, offset) = node.content.find_index(parent_offset);
            path.push(ResolvedPathEntry {
                node: node.clone(),
                index,
                start,
                offset,
            });

            let rem = parent_offset.saturating_sub(offset);
            if rem == 0 || index == node.child_count() {
                break;
            }

            let child = node
                .child(index)
                .expect("index came from find_index")
                .clone();
            if child.is_text() || child.is_leaf() {
                break;
            }

            node = child;
            parent_offset = rem - 1;
            start += offset + 1;
        }

        Ok(ResolvedPos { pos, path })
    }

    /// Create a copy of this node with only the content between the given positions.
    pub fn cut(&self, from: usize, to: usize) -> RichTextResult<Node> {
        if from > to || to > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.content_size(),
            });
        }
        if from == 0 && to == self.content_size() {
            return Ok(self.clone());
        }

        let mut node = self.clone();
        if let Some(text) = &self.text {
            node.text = Some(slice_text(text, from, to));
        } else if self.leaf {
            if from != 0 || to != 0 {
                return Err(RichTextError::Replace(format!(
                    "cannot partially cut leaf node {}",
                    self.name
                )));
            }
        } else {
            node.content = self.content.cut(from, to)?;
        }
        Ok(node)
    }

    /// Create a copy of this node with replacement content.
    pub fn copy_with_content(&self, content: Fragment) -> Node {
        let mut node = self.clone();
        node.content = content;
        node
    }

    /// Cut this node's content into a slice.
    pub fn slice(&self, from: usize, to: usize) -> RichTextResult<Slice> {
        self.slice_with_parents(from, to, false)
    }

    /// Cut this node's content into a slice, optionally preserving parent context.
    pub fn slice_with_parents(
        &self,
        from: usize,
        to: usize,
        include_parents: bool,
    ) -> RichTextResult<Slice> {
        if from > to || to > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.content_size(),
            });
        }
        if from == to {
            return Ok(Slice::empty());
        }

        let from_resolved = self.resolve(from)?;
        let to_resolved = self.resolve(to)?;
        let depth = if include_parents {
            0
        } else {
            from_resolved.shared_depth(to)
        };
        let start = from_resolved
            .start(depth)
            .ok_or_else(|| RichTextError::InvalidPosition {
                pos: from,
                size: self.content_size(),
            })?;
        let node = from_resolved
            .node(depth)
            .ok_or_else(|| RichTextError::InvalidPosition {
                pos: from,
                size: self.content_size(),
            })?;
        let content = node.content.cut(from - start, to - start)?;
        Ok(Slice::new(
            content,
            from_resolved.depth() - depth,
            to_resolved.depth() - depth,
        ))
    }

    /// Get the content-expression cursor at a child index.
    pub fn content_match_at<'a>(
        &self,
        index: usize,
        schema: &'a Schema,
    ) -> RichTextResult<ContentMatch<'a>> {
        if index > self.child_count() {
            return Err(RichTextError::InvalidPosition {
                pos: index,
                size: self.child_count(),
            });
        }
        let node_type = schema.node_type(&self.name)?;
        let prefix = Fragment::from(self.content.as_slice()[..index].to_vec());
        node_type
            .content_expr()
            .match_fragment(schema, &prefix)
            .ok_or_else(|| {
                RichTextError::InvalidContent(format!(
                    "called content_match_at on invalid {} content",
                    self.name
                ))
            })
    }

    /// Test whether replacing child indexes with a fragment would leave valid content.
    pub fn can_replace(
        &self,
        from: usize,
        to: usize,
        replacement: &Fragment,
        schema: &Schema,
    ) -> bool {
        self.can_replace_fragment(from, to, replacement, 0, replacement.len(), schema)
    }

    /// Test whether replacing child indexes with a fragment subrange would be valid.
    pub fn can_replace_fragment(
        &self,
        from: usize,
        to: usize,
        replacement: &Fragment,
        start: usize,
        end: usize,
        schema: &Schema,
    ) -> bool {
        if from > to || to > self.child_count() || start > end || end > replacement.len() {
            return false;
        }
        let Ok(parent_type) = schema.node_type(&self.name) else {
            return false;
        };
        let inserted = Fragment::from(replacement.as_slice()[start..end].to_vec());
        let suffix = Fragment::from(self.content.as_slice()[to..].to_vec());
        let Some(after_insert) = self
            .content_match_at(from, schema)
            .ok()
            .and_then(|content_match| content_match.match_fragment(&inserted))
        else {
            return false;
        };
        let Some(after_suffix) = after_insert.match_fragment(&suffix) else {
            return false;
        };
        if !after_suffix.valid_end() {
            return false;
        }
        replacement.as_slice()[start..end].iter().all(|node| {
            node.marks()
                .iter()
                .all(|mark| parent_type.marks.allows(schema, mark))
        })
    }

    /// Test whether replacing child indexes with a node type would be valid.
    pub fn can_replace_with(
        &self,
        from: usize,
        to: usize,
        type_name: &str,
        marks: &[Mark],
        schema: &Schema,
    ) -> bool {
        let Ok(parent_type) = schema.node_type(&self.name) else {
            return false;
        };
        if !marks
            .iter()
            .all(|mark| parent_type.marks.allows(schema, mark))
        {
            return false;
        }
        let Ok(node_type) = schema.node_type(type_name) else {
            return false;
        };
        let node = Node {
            name: type_name.to_string(),
            attrs: Attrs::new(),
            marks: marks.to_vec(),
            content: Fragment::empty(),
            text: (type_name == "text").then(String::new),
            leaf: node_type.content_expr().is_empty(),
        };
        self.can_replace(from, to, &Fragment::from(node), schema)
    }

    /// Test whether another node's content can be appended to this node.
    pub fn can_append(&self, other: &Node, schema: &Schema) -> bool {
        if other.content_size() > 0 {
            self.can_replace(
                self.child_count(),
                self.child_count(),
                other.content(),
                schema,
            )
        } else {
            self.compatible_content(other, schema)
        }
    }

    /// Test whether two node types have compatible content expressions.
    pub fn compatible_content(&self, other: &Node, schema: &Schema) -> bool {
        let Ok(left_type) = schema.node_type(&self.name) else {
            return false;
        };
        let Ok(right_type) = schema.node_type(other.type_name()) else {
            return false;
        };
        left_type.content_expr() == right_type.content_expr()
    }

    /// Visit all descendant nodes in document order.
    pub fn descendants<F>(&self, mut f: F)
    where
        F: FnMut(&Node, usize),
    {
        self.nodes_between(0, self.content.size(), |node, pos, _, _| {
            f(node, pos);
            true
        })
        .expect("full content range is valid");
    }

    /// Visit descendant nodes that overlap a content-position range.
    pub fn nodes_between<F>(&self, from: usize, to: usize, mut f: F) -> RichTextResult<()>
    where
        F: FnMut(&Node, usize, Option<&Node>, usize) -> bool,
    {
        if from > to || to > self.content_size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.content_size(),
            });
        }
        self.content
            .nodes_between_inner(from, to, &mut f, 0, Some(self));
        Ok(())
    }

    /// Test whether a mark occurs in a content-position range.
    pub fn range_has_mark(&self, from: usize, to: usize, mark: &Mark) -> RichTextResult<bool> {
        let mut found = false;
        if to > from {
            self.nodes_between(from, to, |node, _, _, _| {
                if mark.is_in_set(node.marks()) {
                    found = true;
                    false
                } else {
                    true
                }
            })?;
        }
        Ok(found)
    }

    /// Test whether a mark type occurs in a content-position range.
    pub fn range_has_mark_type(
        &self,
        from: usize,
        to: usize,
        mark_type: &str,
    ) -> RichTextResult<bool> {
        let mut found = false;
        if to > from {
            self.nodes_between(from, to, |node, _, _, _| {
                if node
                    .marks()
                    .iter()
                    .any(|mark| mark.type_name() == mark_type)
                {
                    found = true;
                    false
                } else {
                    true
                }
            })?;
        }
        Ok(found)
    }

    /// Render this node in PM's `toString` debugging format. Block / wrapper
    /// nodes render as `name(child1, child2)`; leaf nodes render as `name`;
    /// text nodes render their JSON-quoted text; marks wrap the inner string
    /// like `em(strong("text"))` from outer to inner.
    pub fn to_debug_string(&self) -> String {
        let inner = if self.is_text() {
            format!("{:?}", self.text.as_deref().unwrap_or(""))
        } else if self.content.size() > 0 {
            format!("{}({})", self.name, self.content.to_inner_string())
        } else {
            self.name.clone()
        };
        wrap_marks_in_debug(&self.marks, inner)
    }
}

fn wrap_marks_in_debug(marks: &[Mark], mut inner: String) -> String {
    for mark in marks.iter().rev() {
        inner = format!("{}({})", mark.name, inner);
    }
    inner
}

impl std::fmt::Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_debug_string())
    }
}

impl Serialize for Node {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut len = 1;
        if !self.attrs.is_empty() {
            len += 1;
        }
        if !self.marks.is_empty() {
            len += 1;
        }
        if self.text.is_none() {
            len += 1;
        }
        if self.text.is_some() {
            len += 1;
        }
        if !self.content.is_empty() {
            len += 1;
        }

        let mut state = serializer.serialize_struct("Node", len)?;
        state.serialize_field("type", &self.name)?;
        if !self.attrs.is_empty() {
            state.serialize_field("attrs", &self.attrs)?;
        }
        if !self.marks.is_empty() {
            state.serialize_field("marks", &self.marks)?;
        }
        if self.text.is_none() {
            state.serialize_field("leaf", &self.leaf)?;
        }
        if let Some(text) = &self.text {
            state.serialize_field("text", text)?;
        }
        if !self.content.is_empty() {
            state.serialize_field("content", self.content.as_slice())?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct NodeJson {
            #[serde(rename = "type")]
            name: String,
            #[serde(default)]
            attrs: Attrs,
            #[serde(default)]
            marks: Vec<Mark>,
            #[serde(default)]
            content: Vec<Node>,
            text: Option<String>,
            #[serde(default)]
            leaf: bool,
        }

        let value = NodeJson::deserialize(deserializer)?;
        if value.text.is_some() && !value.content.is_empty() {
            return Err(de::Error::custom(
                "node JSON cannot contain both text and content",
            ));
        }

        Ok(Self {
            name: value.name,
            attrs: value.attrs,
            marks: value.marks,
            content: Fragment::from(value.content),
            text: value.text,
            leaf: value.leaf,
        })
    }
}

/// An ordered list of child nodes.
///
/// Children are stored behind an `Arc` so clones are refcount bumps rather
/// than deep tree copies. Mutating methods use `Arc::make_mut` (copy-on-write).
#[derive(Clone, Debug, Default)]
pub struct Fragment {
    pub(crate) children: Arc<Vec<Node>>,
}

impl PartialEq for Fragment {
    /// `Arc::ptr_eq` fast-path — Fragments that share the same
    /// `Arc<Vec<Node>>` are structurally equal by construction.
    /// The reconciler's `if old_node == new_node` short-circuit
    /// at the top of every `reconcile_node` call is what makes
    /// this matter: when a transaction modifies one paragraph
    /// in a 500-paragraph doc, all the other 499 paragraphs
    /// land in the new tree as Arc clones. Without this
    /// fast-path, comparing each unchanged subtree walked
    /// recursively through every text leaf. With it, each
    /// subtree comparison is one pointer compare.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.children, &other.children) || self.children == other.children
    }
}

impl Fragment {
    /// Empty fragment.
    pub fn empty() -> Self {
        Self {
            children: Arc::new(Vec::new()),
        }
    }

    /// Construct a fragment from child nodes.
    pub fn new(children: Vec<Node>) -> Self {
        let mut fragment = Self {
            children: Arc::new(children),
        };
        fragment.merge_adjacent_text();
        fragment
    }

    /// Number of children.
    pub fn len(&self) -> usize {
        self.children.len()
    }

    /// Whether there are no children.
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Child slice.
    pub fn as_slice(&self) -> &[Node] {
        self.children.as_slice()
    }

    /// Iterate child nodes.
    pub fn iter(&self) -> std::slice::Iter<'_, Node> {
        self.children.iter()
    }

    /// Get a child node.
    pub fn child(&self, index: usize) -> Option<&Node> {
        self.children.get(index)
    }

    /// Slice this fragment by child index, mirroring upstream `Fragment.cutByIndex`.
    /// Used by structure predicates (lift target, can-split) to reason about the
    /// sub-range of children inside a `NodeRange` without re-walking by content
    /// positions.
    pub fn cut_by_index(&self, from: usize, to: usize) -> Fragment {
        if from == 0 && to == self.children.len() {
            return self.clone();
        }
        let from = from.min(self.children.len());
        let to = to.min(self.children.len()).max(from);
        Fragment::from(self.children.as_slice()[from..to].to_vec())
    }

    /// Total content size.
    pub fn size(&self) -> usize {
        self.children.iter().map(Node::node_size).sum()
    }

    /// Append a child.
    pub fn push(&mut self, child: Node) {
        Arc::make_mut(&mut self.children).push(child);
        self.merge_adjacent_text();
    }

    /// Get mutable access to the underlying children vec, performing a
    /// copy-on-write of the shared `Arc` if needed.
    pub(crate) fn children_mut(&mut self) -> &mut Vec<Node> {
        Arc::make_mut(&mut self.children)
    }

    /// Produce a new fragment with the child at `index` replaced. Cheaper
    /// than `as_slice().to_vec()` + reassign because the rest of the children
    /// stay behind the shared `Arc` (a real clone only happens when other
    /// handles exist).
    pub fn replace_child(&self, index: usize, node: Node) -> RichTextResult<Fragment> {
        if index >= self.children.len() {
            return Err(RichTextError::InvalidPosition {
                pos: index,
                size: self.children.len(),
            });
        }
        let mut next = self.clone();
        Arc::make_mut(&mut next.children)[index] = node;
        Ok(next)
    }

    /// Append another fragment.
    pub fn append(mut self, other: Fragment) -> Self {
        let other_children = Arc::try_unwrap(other.children).unwrap_or_else(|arc| (*arc).clone());
        Arc::make_mut(&mut self.children).extend(other_children);
        self.merge_adjacent_text();
        self
    }

    /// Find the child index and child start offset for a content position.
    pub fn find_index(&self, pos: usize) -> (usize, usize) {
        let mut offset = 0;
        for (index, child) in self.children.iter().enumerate() {
            if offset == pos {
                return (index, offset);
            }
            let end = offset + child.node_size();
            if end > pos {
                return (index, offset);
            }
            offset = end;
        }
        (self.children.len(), offset)
    }

    /// Visit descendant nodes that overlap a content-position range.
    pub fn nodes_between<F>(&self, from: usize, to: usize, mut f: F) -> RichTextResult<()>
    where
        F: FnMut(&Node, usize, Option<&Node>, usize) -> bool,
    {
        if from > to || to > self.size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.size(),
            });
        }
        self.nodes_between_inner(from, to, &mut f, 0, None);
        Ok(())
    }

    /// Visit all descendants in document order.
    pub fn descendants<F>(&self, f: F) -> RichTextResult<()>
    where
        F: FnMut(&Node, usize, Option<&Node>, usize) -> bool,
    {
        self.nodes_between(0, self.size(), f)
    }

    fn nodes_between_inner<F>(
        &self,
        from: usize,
        to: usize,
        f: &mut F,
        node_start: usize,
        parent: Option<&Node>,
    ) where
        F: FnMut(&Node, usize, Option<&Node>, usize) -> bool,
    {
        let mut offset = 0;
        for (index, child) in self.children.iter().enumerate() {
            if offset >= to {
                break;
            }

            let end = offset + child.node_size();
            if end > from
                && f(child, node_start + offset, parent, index)
                && !child.is_text()
                && !child.is_leaf()
                && !child.content.is_empty()
            {
                let start = offset + 1;
                child.content.nodes_between_inner(
                    from.saturating_sub(start),
                    to.saturating_sub(start).min(child.content.size()),
                    f,
                    node_start + start,
                    Some(child),
                );
            }
            offset = end;
        }
    }

    /// Cut a content-position range out of this fragment.
    pub fn cut(&self, from: usize, to: usize) -> RichTextResult<Fragment> {
        if from > to || to > self.size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.size(),
            });
        }
        if from == 0 && to == self.size() {
            return Ok(self.clone());
        }

        let mut out = Vec::new();
        let mut offset = 0;

        for child in self.children.iter() {
            let end = offset + child.node_size();
            if offset >= to {
                break;
            }
            if end <= from {
                offset = end;
                continue;
            }

            if offset >= from && end <= to {
                out.push(child.clone());
            } else {
                let child_from = from.saturating_sub(offset);
                let child_to = to.saturating_sub(offset).min(child.node_size());
                let next = if child.is_text() {
                    child.cut(child_from, child_to)?
                } else {
                    let content_from = child_from.saturating_sub(1).min(child.content_size());
                    let content_to = child_to.saturating_sub(1).min(child.content_size());
                    child.cut(content_from, content_to)?
                };
                if next.node_size() > 0 {
                    out.push(next);
                }
            }

            offset = end;
        }

        let mut fragment = Fragment::new(out);
        fragment.merge_adjacent_text();
        Ok(fragment)
    }

    /// Replace a range with another fragment.
    pub fn replace_between(
        &self,
        from: usize,
        to: usize,
        replacement: Fragment,
    ) -> RichTextResult<Fragment> {
        if from > to || to > self.size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.size(),
            });
        }

        let left = self.cut(0, from)?;
        let right = self.cut(to, self.size())?;
        let mut out: Vec<Node> = Vec::with_capacity(
            left.children.len() + replacement.children.len() + right.children.len(),
        );
        out.extend(left.children.iter().cloned());
        out.extend(replacement.children.iter().cloned());
        out.extend(right.children.iter().cloned());
        Ok(Fragment::from(out))
    }

    /// Apply or remove a mark in a content-position range.
    pub fn apply_mark_between(
        &self,
        from: usize,
        to: usize,
        mark: &Mark,
        add: bool,
        schema: &Schema,
    ) -> RichTextResult<Fragment> {
        if from > to || to > self.size() {
            return Err(RichTextError::InvalidPosition {
                pos: to,
                size: self.size(),
            });
        }
        if from == to {
            return Ok(self.clone());
        }

        let mut out = Vec::new();
        let mut offset = 0;

        for child in self.children.iter() {
            let size = child.node_size();
            let end = offset + size;
            if end <= from || offset >= to {
                out.push(child.clone());
                offset = end;
                continue;
            }

            let overlap_from = from.saturating_sub(offset);
            let overlap_to = to.saturating_sub(offset).min(size);
            if child.is_text() {
                let text = child.text.as_ref().expect("text node");
                if overlap_from > 0 {
                    let mut before = child.clone();
                    before.text = Some(slice_text(text, 0, overlap_from));
                    if before.text.as_ref().is_some_and(|s| !s.is_empty()) {
                        out.push(before);
                    }
                }

                let mut middle = child.clone();
                middle.text = Some(slice_text(text, overlap_from, overlap_to));
                if add {
                    middle.marks = schema.add_mark_to_set(&middle.marks, mark.clone())?;
                } else {
                    middle.marks.retain(|existing| existing != mark);
                }
                if middle.text.as_ref().is_some_and(|s| !s.is_empty()) {
                    out.push(middle);
                }

                if overlap_to < size {
                    let mut after = child.clone();
                    after.text = Some(slice_text(text, overlap_to, size));
                    if after.text.as_ref().is_some_and(|s| !s.is_empty()) {
                        out.push(after);
                    }
                }
            } else if child.is_leaf() {
                let mut next = child.clone();
                if overlap_from == 0 && overlap_to == size {
                    if add {
                        next.marks = schema.add_mark_to_set(&next.marks, mark.clone())?;
                    } else {
                        next.marks.retain(|existing| existing != mark);
                    }
                }
                out.push(next);
            } else {
                let mut next = child.clone();
                let inner_from = overlap_from.saturating_sub(1);
                let inner_to = overlap_to.saturating_sub(1).min(child.content.size());
                next.content = child
                    .content
                    .apply_mark_between(inner_from, inner_to, mark, add, schema)?;
                out.push(next);
            }
            offset = end;
        }

        let mut fragment = Fragment::new(out);
        fragment.merge_adjacent_text();
        Ok(fragment)
    }

    /// Concatenate descendant text in a content-position range.
    pub fn text_between(&self, from: usize, to: usize, block_separator: &str) -> String {
        self.text_between_with(from, to, block_separator, None)
    }

    /// Like `text_between` but with optional leaf-text production.
    /// See `Node::text_between_with` for the contract.
    pub fn text_between_with(
        &self,
        from: usize,
        to: usize,
        block_separator: &str,
        leaf_text: Option<&dyn Fn(&Node) -> String>,
    ) -> String {
        let mut text = String::new();
        let mut offset = 0;
        let mut separated = false;

        for child in self.children.iter() {
            let end = offset + child.node_size();
            if end <= from || offset >= to {
                offset = end;
                continue;
            }

            if child.is_text() {
                let child_from = from.saturating_sub(offset);
                let child_to = to.saturating_sub(offset).min(child.node_size());
                if let Some(child_text) = &child.text {
                    text.push_str(&slice_text(child_text, child_from, child_to));
                }
            } else if child.is_leaf() {
                if let Some(leaf_text) = leaf_text {
                    text.push_str(&leaf_text(child));
                }
            } else {
                if separated && !block_separator.is_empty() {
                    text.push_str(block_separator);
                }
                let child_from = from.saturating_sub(offset + 1);
                let child_to = to.saturating_sub(offset + 1).min(child.content.size());
                text.push_str(&child.content.text_between_with(
                    child_from,
                    child_to,
                    block_separator,
                    leaf_text,
                ));
                separated = true;
            }

            offset = end;
        }

        text
    }

    pub(crate) fn merge_adjacent_text(&mut self) {
        // Fast path: nothing to merge if there are fewer than two children, or
        // no adjacent pair where both sides are text with matching markup.
        let needs_merge = self
            .children
            .windows(2)
            .any(|pair| can_merge_text_pair(&pair[0], &pair[1]));
        if !needs_merge {
            return;
        }

        let children = Arc::make_mut(&mut self.children);
        let mut merged: Vec<Node> = Vec::with_capacity(children.len());
        for child in children.drain(..) {
            if let Some(last) = merged.last_mut()
                && last.is_text()
                && child.is_text()
                && last.marks == child.marks
                && last.attrs == child.attrs
                && let (Some(left), Some(right)) = (&mut last.text, &child.text)
            {
                left.push_str(right);
                continue;
            }
            merged.push(child);
        }
        *children = merged;
    }

    /// Render this fragment's children as a comma-separated debug string —
    /// the inside of PM's `toString` parens.
    pub fn to_inner_string(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(self.children.len());
        for child in self.children.iter() {
            parts.push(child.to_debug_string());
        }
        parts.join(", ")
    }
}

impl std::fmt::Display for Fragment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<{}>", self.to_inner_string())
    }
}

fn can_merge_text_pair(left: &Node, right: &Node) -> bool {
    left.is_text() && right.is_text() && left.marks == right.marks && left.attrs == right.attrs
}

impl From<Vec<Node>> for Fragment {
    fn from(value: Vec<Node>) -> Self {
        let mut fragment = Self {
            children: Arc::new(value),
        };
        fragment.merge_adjacent_text();
        fragment
    }
}

impl From<Node> for Fragment {
    fn from(value: Node) -> Self {
        Self {
            children: Arc::new(vec![value]),
        }
    }
}

impl IntoIterator for Fragment {
    type IntoIter = std::vec::IntoIter<Node>;
    type Item = Node;

    fn into_iter(self) -> Self::IntoIter {
        Arc::try_unwrap(self.children)
            .unwrap_or_else(|arc| (*arc).clone())
            .into_iter()
    }
}

/// A slice of document content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Slice {
    /// Slice content.
    pub content: Fragment,
    /// Number of open nodes at the start (camelCased in JSON to match
    /// upstream ProseMirror).
    #[serde(rename = "openStart", default)]
    pub open_start: usize,
    /// Number of open nodes at the end.
    #[serde(rename = "openEnd", default)]
    pub open_end: usize,
}

impl Slice {
    /// Construct a slice.
    pub fn new(content: Fragment, open_start: usize, open_end: usize) -> Self {
        Self {
            content,
            open_start,
            open_end,
        }
    }

    /// Empty slice.
    pub fn empty() -> Self {
        Self::new(Fragment::empty(), 0, 0)
    }

    /// Slice size measured in content positions.
    pub fn size(&self) -> usize {
        self.content
            .size()
            .saturating_sub(self.open_start + self.open_end)
    }

    /// Remove a range from this slice's content, preserving open depths.
    pub fn remove_between(&self, from: usize, to: usize) -> RichTextResult<Self> {
        let from = from + self.open_start;
        let to = to + self.open_start;
        Ok(Self {
            content: self.content.replace_between(from, to, Fragment::empty())?,
            open_start: self.open_start,
            open_end: self.open_end,
        })
    }
}

impl Serialize for Fragment {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.children.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Fragment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let children = Vec::<Node>::deserialize(deserializer)?;
        Ok(Fragment::from(children))
    }
}

/// One entry in a resolved position path.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPathEntry {
    /// Node at this depth.
    pub node: Node,
    /// Child index at this depth.
    pub index: usize,
    /// Absolute position where this node's content starts.
    pub start: usize,
    /// Offset of the child at `index` inside this node's content.
    pub offset: usize,
}

/// A resolved document position.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPos {
    pos: usize,
    path: Vec<ResolvedPathEntry>,
}

impl ResolvedPos {
    /// Absolute position.
    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Resolved depth.
    pub fn depth(&self) -> usize {
        self.path.len().saturating_sub(1)
    }

    /// Node at a depth.
    pub fn node(&self, depth: usize) -> Option<&Node> {
        self.path.get(depth).map(|entry| &entry.node)
    }

    /// Parent node at the deepest resolved depth.
    pub fn parent(&self) -> &Node {
        &self.path.last().expect("resolved path is never empty").node
    }

    /// Child index at a depth.
    pub fn index(&self, depth: usize) -> Option<usize> {
        self.path.get(depth).map(|entry| entry.index)
    }

    /// Child index after this position at a depth.
    pub fn index_after(&self, depth: usize) -> Option<usize> {
        let entry = self.path.get(depth)?;
        let extra = usize::from(!(depth == self.depth() && self.text_offset() == 0));
        Some((entry.index + extra).min(entry.node.child_count()))
    }

    /// Start position for the node at a depth.
    pub fn start(&self, depth: usize) -> Option<usize> {
        self.path.get(depth).map(|entry| entry.start)
    }

    /// End position for the node at a depth.
    pub fn end(&self, depth: usize) -> Option<usize> {
        self.path
            .get(depth)
            .map(|entry| entry.start + entry.node.content_size())
    }

    /// Position before the node at a depth.
    pub fn before(&self, depth: usize) -> Option<usize> {
        if depth == self.depth() + 1 {
            return Some(self.pos);
        }
        self.path
            .get(depth)
            .and_then(|entry| entry.start.checked_sub(1))
    }

    /// Position after the node at a depth.
    pub fn after(&self, depth: usize) -> Option<usize> {
        if depth == self.depth() + 1 {
            return Some(self.pos);
        }
        self.path
            .get(depth)
            .map(|entry| entry.start + entry.node.content_size() + usize::from(depth > 0))
            .filter(|_| depth > 0)
    }

    /// Offset into the parent node.
    pub fn parent_offset(&self) -> usize {
        self.pos
            - self
                .path
                .last()
                .expect("resolved path is never empty")
                .start
    }

    /// Offset into the current text node, or zero when between nodes.
    pub fn text_offset(&self) -> usize {
        let entry = self.path.last().expect("resolved path is never empty");
        self.parent_offset().saturating_sub(entry.offset)
    }

    /// Node directly before this position, cut when the position is inside text.
    pub fn node_before(&self) -> Option<Node> {
        let entry = self.path.last()?;
        let index = entry.index;
        let text_offset = self.text_offset();
        if text_offset != 0 {
            return entry
                .node
                .child(index)
                .and_then(|child| child.cut(0, text_offset).ok());
        }
        index
            .checked_sub(1)
            .and_then(|before_index| entry.node.child(before_index).cloned())
    }

    /// Node directly after this position, cut when the position is inside text.
    pub fn node_after(&self) -> Option<Node> {
        let entry = self.path.last()?;
        if entry.index == entry.node.child_count() {
            return None;
        }

        let child = entry.node.child(entry.index)?;
        let text_offset = self.text_offset();
        if text_offset == 0 {
            Some(child.clone())
        } else {
            child.cut(text_offset, child.content_size()).ok()
        }
    }

    /// Active marks at this position.
    pub fn marks(&self, schema: &Schema) -> Vec<Mark> {
        let parent = self.parent();
        if parent.content_size() == 0 {
            return Vec::new();
        }

        let index = self.index(self.depth()).unwrap_or(0);
        if self.text_offset() != 0 {
            return parent
                .child(index)
                .map(|child| child.marks().to_vec())
                .unwrap_or_default();
        }

        let mut main = index.checked_sub(1).and_then(|before| parent.child(before));
        let mut other = parent.child(index);
        if main.is_none() {
            std::mem::swap(&mut main, &mut other);
        }

        let Some(main) = main else {
            return Vec::new();
        };
        let mut marks = main.marks().to_vec();
        marks.retain(|mark| {
            schema
                .mark_type(mark.type_name())
                .is_ok_and(MarkType::is_inclusive)
                || other.is_some_and(|other| mark.is_in_set(other.marks()))
        });
        marks
    }

    /// Marks across a deleted range that should be preserved after deletion.
    pub fn marks_across(&self, end: &ResolvedPos, schema: &Schema) -> Option<Vec<Mark>> {
        let after = self.parent().child(self.index(self.depth())?)?;
        if !after.is_text()
            && !schema
                .node_type(after.type_name())
                .is_ok_and(NodeType::is_inline)
        {
            return None;
        }

        let next = end.parent().child(end.index(end.depth())?);
        let mut marks = after.marks().to_vec();
        marks.retain(|mark| {
            schema
                .mark_type(mark.type_name())
                .is_ok_and(MarkType::is_inclusive)
                || next.is_some_and(|next| mark.is_in_set(next.marks()))
        });
        Some(marks)
    }

    /// Absolute position at a child index inside the node at a depth.
    pub fn pos_at_index(&self, index: usize, depth: usize) -> Option<usize> {
        let entry = self.path.get(depth)?;
        if index > entry.node.child_count() {
            return None;
        }
        let mut pos = entry.start;
        for child_index in 0..index {
            pos += entry.node.child(child_index)?.node_size();
        }
        Some(pos)
    }

    /// Deepest depth whose node contains the given position.
    pub fn shared_depth(&self, pos: usize) -> usize {
        for depth in (1..=self.depth()).rev() {
            if self.start(depth).is_some_and(|start| start <= pos)
                && self.end(depth).is_some_and(|end| end >= pos)
            {
                return depth;
            }
        }
        0
    }

    /// Whether two resolved positions point into the same parent.
    pub fn same_parent(&self, other: &ResolvedPos) -> bool {
        self.pos.saturating_sub(self.parent_offset())
            == other.pos.saturating_sub(other.parent_offset())
    }

    /// Create a flat range around block content between two positions.
    pub fn block_range(&self, other: Option<&ResolvedPos>) -> Option<NodeRange> {
        self.block_range_with(other, |_| true)
    }

    /// Create a flat range around block content between two positions, walking
    /// outward until the parent at the candidate depth satisfies `predicate`.
    /// Matches upstream `ResolvedPos.blockRange(other, pred)` so callers can
    /// constrain the range to specific wrapper types.
    pub fn block_range_with<F>(
        &self,
        other: Option<&ResolvedPos>,
        predicate: F,
    ) -> Option<NodeRange>
    where
        F: Fn(&Node) -> bool,
    {
        let other = other.unwrap_or(self);
        if other.pos < self.pos {
            return other.block_range_with(Some(self), predicate);
        }

        let mut depth = self.depth();
        if self.same_parent(other) && depth > 0 && self.parent().content.iter().any(Node::is_text) {
            depth -= 1;
        }

        loop {
            let parent = self.node(depth)?;
            if other.pos <= self.end(depth)? && predicate(parent) {
                return Some(NodeRange {
                    from: self.clone(),
                    to: other.clone(),
                    depth,
                });
            }
            if depth == 0 {
                return None;
            }
            depth -= 1;
        }
    }

    /// Full path entries.
    pub fn path(&self) -> &[ResolvedPathEntry] {
        &self.path
    }
}

/// A flat range of sibling nodes.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRange {
    from: ResolvedPos,
    to: ResolvedPos,
    depth: usize,
}

impl NodeRange {
    /// Position at the start of the range.
    pub fn start(&self) -> usize {
        self.from.before(self.depth + 1).unwrap_or(self.from.pos())
    }

    /// Position at the end of the range.
    pub fn end(&self) -> usize {
        self.to.after(self.depth + 1).unwrap_or(self.to.pos())
    }

    /// Parent node that contains this range.
    pub fn parent(&self) -> &Node {
        self.from
            .node(self.depth)
            .expect("range depth came from resolved position")
    }

    /// Range depth.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Start child index in the range parent.
    pub fn start_index(&self) -> usize {
        self.from.index(self.depth).unwrap_or(0)
    }

    /// End child index in the range parent.
    pub fn end_index(&self) -> usize {
        self.to
            .index_after(self.depth)
            .unwrap_or_else(|| self.parent().child_count())
    }

    /// Resolved start position. Mirrors upstream `range.$from`.
    pub fn from_resolved(&self) -> &ResolvedPos {
        &self.from
    }

    /// Resolved end position. Mirrors upstream `range.$to`.
    pub fn to_resolved(&self) -> &ResolvedPos {
        &self.to
    }
}

/// Find the first differing position between two fragments.
pub fn find_diff_start(a: &Fragment, b: &Fragment, pos: usize) -> Option<usize> {
    if a == b {
        return None;
    }

    let mut offset = pos;
    let mut left = a.iter();
    let mut right = b.iter();

    loop {
        match (left.next(), right.next()) {
            (Some(l), Some(r)) if l == r => offset += l.node_size(),
            (Some(l), Some(r)) if l.is_text() && r.is_text() => {
                if l.marks != r.marks || l.attrs != r.attrs {
                    return Some(offset);
                }
                let lt = l.text().unwrap_or_default();
                let rt = r.text().unwrap_or_default();
                let common = lt
                    .chars()
                    .zip(rt.chars())
                    .take_while(|(x, y)| x == y)
                    .count();
                return Some(offset + common);
            }
            (Some(l), Some(r))
                if !l.is_text()
                    && !r.is_text()
                    && l.name == r.name
                    && (l.attrs != r.attrs || l.marks != r.marks) =>
            {
                return Some(offset);
            }
            (Some(l), Some(r)) if !l.is_text() && !r.is_text() && l.name == r.name => {
                return find_diff_start(&l.content, &r.content, offset + 1).or(Some(offset));
            }
            _ => return Some(offset),
        }
    }
}

/// Find the trailing differing range between two fragments.
pub fn find_diff_end(
    a: &Fragment,
    b: &Fragment,
    pos_a: usize,
    pos_b: usize,
) -> Option<(usize, usize)> {
    if a == b {
        return None;
    }

    let mut end_a = pos_a + a.size();
    let mut end_b = pos_b + b.size();
    let mut left = a.as_slice().iter().rev();
    let mut right = b.as_slice().iter().rev();

    loop {
        match (left.next(), right.next()) {
            (Some(l), Some(r)) if l == r => {
                end_a -= l.node_size();
                end_b -= r.node_size();
            }
            (Some(l), Some(r)) if l.is_text() && r.is_text() => {
                // PM's `sameMarkup` returns false when marks or attrs differ —
                // the whole text node is part of the diff, so the diff ends at
                // the position AFTER it (i.e., the current end_a/end_b, no
                // decrement). Only when the markup matches do we walk chars
                // from the end to narrow the diff to character-level.
                if l.marks != r.marks || l.attrs != r.attrs {
                    return Some((end_a, end_b));
                }
                let common = l
                    .text()
                    .unwrap_or_default()
                    .chars()
                    .rev()
                    .zip(r.text().unwrap_or_default().chars().rev())
                    .take_while(|(x, y)| x == y)
                    .count();
                return Some((end_a - common, end_b - common));
            }
            (Some(l), Some(r))
                if !l.is_text()
                    && !r.is_text()
                    && l.name == r.name
                    && (l.attrs != r.attrs || l.marks != r.marks) =>
            {
                // Same node type but differing attrs/marks ⇒ whole node is part
                // of the diff. The diff ends at the position AFTER it.
                return Some((end_a, end_b));
            }
            (Some(l), Some(r)) if !l.is_text() && !r.is_text() && l.name == r.name => {
                let content_start_a = end_a - l.node_size() + 1;
                let content_start_b = end_b - r.node_size() + 1;
                return find_diff_end(&l.content, &r.content, content_start_a, content_start_b)
                    .or(Some((end_a - l.node_size(), end_b - r.node_size())));
            }
            _ => return Some((end_a, end_b)),
        }
    }
}

pub(crate) fn text_len(text: &str) -> usize {
    text.chars().count()
}

pub(crate) fn slice_text(text: &str, from: usize, to: usize) -> String {
    text.chars().skip(from).take(to - from).collect()
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}

fn split_names(input: &str) -> impl Iterator<Item = String> + '_ {
    input
        .split_whitespace()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
}

/// Fill missing optional attributes with their declared defaults and reject
/// caller-supplied attribute maps that omit a required attribute. When the
/// spec declares no attributes the caller's map is passed through unchanged
/// for backwards compatibility with pre-AttributeSpec call sites.
fn resolve_attrs(
    spec: &BTreeMap<String, AttributeSpec>,
    attrs: Attrs,
    owner: &str,
) -> RichTextResult<Attrs> {
    if spec.is_empty() {
        return Ok(attrs);
    }
    let mut resolved = Attrs::new();
    for (name, attr_spec) in spec {
        if let Some(value) = attrs.get(name) {
            resolved.insert(name.clone(), value.clone());
            continue;
        }
        match attr_spec.default_value() {
            Some(default) => {
                resolved.insert(name.clone(), default.clone());
            }
            None => {
                return Err(RichTextError::InvalidContent(format!(
                    "missing required attribute {name} on {owner}"
                )));
            }
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Schema {
        Schema::builder()
            .node(NodeSpec::new("doc").content("block+"))
            .node(NodeSpec::new("paragraph").group("block").content("inline*"))
            .node(NodeSpec::new("text").group("inline").inline())
            .mark(MarkSpec::new("strong"))
            .finish()
            .unwrap()
    }

    #[test]
    fn validates_basic_content_expression() {
        let schema = schema();
        let text = schema.text("hello", Vec::new()).unwrap();
        let paragraph = schema
            .node("paragraph", Attrs::new(), Fragment::from(text))
            .unwrap();
        let doc = schema
            .node("doc", Attrs::new(), Fragment::from(paragraph))
            .unwrap();

        assert_eq!(doc.text_content(), "hello");
        assert_eq!(doc.content_size(), 7);
    }

    #[test]
    fn rejects_invalid_child_type() {
        let schema = schema();
        let text = schema.text("bad", Vec::new()).unwrap();
        let err = schema
            .node("doc", Attrs::new(), Fragment::from(text))
            .unwrap_err();
        assert!(matches!(err, RichTextError::InvalidContent(_)));
    }

    #[test]
    fn round_trips_node_json() {
        let schema = schema();
        let mark = schema.mark("strong", Attrs::new()).unwrap();
        let text = schema.text("hello", vec![mark]).unwrap();
        let paragraph = schema
            .node("paragraph", Attrs::new(), Fragment::from(text))
            .unwrap();
        let doc = schema
            .node("doc", Attrs::new(), Fragment::from(paragraph))
            .unwrap();

        let json = serde_json::to_value(&doc).unwrap();
        let decoded: Node = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, doc);
        schema.check_node(&decoded).unwrap();
    }

    #[test]
    fn empty_container_nodes_are_not_leaf_nodes() {
        let schema = schema();
        let paragraph = schema
            .node("paragraph", Attrs::new(), Fragment::empty())
            .unwrap();

        assert!(!paragraph.is_leaf());
        assert_eq!(paragraph.node_size(), 2);
        assert_eq!(paragraph.content_size(), 0);
    }

    #[test]
    fn content_match_default_textblock_uses_outgoing_terms() {
        let schema = Schema::builder()
            .node(NodeSpec::new("doc").content("title block*"))
            .node(NodeSpec::new("title").group("title").content("inline*"))
            .node(
                NodeSpec::new("required_block")
                    .group("block")
                    .content("inline*")
                    .required_attr("kind"),
            )
            .node(NodeSpec::new("paragraph").group("block").content("inline*"))
            .node(NodeSpec::new("heading").group("block").content("inline*"))
            .node(NodeSpec::new("text").group("inline").inline())
            .finish()
            .unwrap();
        let title = schema
            .node(
                "title",
                Attrs::new(),
                Fragment::from(schema.text("Draft", Vec::new()).unwrap()),
            )
            .unwrap();
        let doc = schema
            .node("doc", Attrs::new(), Fragment::from(title))
            .unwrap();

        let content_match = doc.content_match_at(1, &schema).unwrap();
        assert!(
            content_match
                .match_type(schema.node_type("title").unwrap())
                .is_none()
        );
        assert_eq!(
            content_match.default_textblock_type().map(NodeType::name),
            Some("paragraph")
        );
    }

    #[test]
    fn add_mark_to_set_replaces_excluded_instances_and_respects_rank() {
        let schema = Schema::builder()
            .node(NodeSpec::new("doc").content("paragraph+"))
            .node(NodeSpec::new("paragraph").content("text*"))
            .node(NodeSpec::new("text").inline())
            .mark(MarkSpec::new("link"))
            .mark(MarkSpec::new("em"))
            .mark(MarkSpec::new("code").excludes("_"))
            .mark(MarkSpec::new("strong").excludes("em-group"))
            .mark(MarkSpec::new("custom_em").group("em-group").excludes(""))
            .finish()
            .unwrap();

        let link_a = schema
            .mark(
                "link",
                [("href".to_string(), Value::String("a".to_string()))].into(),
            )
            .unwrap();
        let link_b = schema
            .mark(
                "link",
                [("href".to_string(), Value::String("b".to_string()))].into(),
            )
            .unwrap();
        let em = schema.mark("em", Attrs::new()).unwrap();
        let code = schema.mark("code", Attrs::new()).unwrap();
        let strong = schema.mark("strong", Attrs::new()).unwrap();
        let custom_em = schema.mark("custom_em", Attrs::new()).unwrap();

        assert_eq!(
            schema
                .add_mark_to_set(&[link_a, em.clone()], link_b.clone())
                .unwrap(),
            vec![link_b, em.clone()]
        );
        assert_eq!(
            schema
                .add_mark_to_set(std::slice::from_ref(&em), code.clone())
                .unwrap(),
            vec![code.clone()]
        );
        assert_eq!(
            schema
                .add_mark_to_set(std::slice::from_ref(&code), em.clone())
                .unwrap(),
            vec![code]
        );
        assert_eq!(
            schema
                .add_mark_to_set(std::slice::from_ref(&custom_em), strong.clone())
                .unwrap(),
            vec![strong.clone()]
        );
        assert_eq!(
            schema
                .add_mark_to_set(std::slice::from_ref(&strong), custom_em)
                .unwrap(),
            vec![strong]
        );
    }
}
