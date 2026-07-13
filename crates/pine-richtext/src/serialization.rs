//! Typed semantic-node serialization policies and model-only exporters.
//!
//! These APIs deliberately operate on [`Node`] trees. They never inspect the
//! browser editor surface or a component node-view DOM, so interactive chrome
//! cannot leak into persisted HTML, text, or clipboard data.

use std::any::TypeId;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Fragment, Node, Slice};
use crate::render::{DomElementSpec, DomFragmentSpec, DomNodeSpec, DomOutputError, NodeDomSpec};
use crate::runtime::EditorRuntime;
use crate::{RichTextNodeType, WireNode};

/// Explicit Markdown support for one typed semantic node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownPolicy {
    /// The extension contributes a semantic emitter (and any import rules its
    /// format needs). Runtime construction verifies the emitter exists.
    Supported,
    /// Markdown cannot represent this semantic node without unacceptable
    /// loss. Export fails instead of silently dropping the node.
    Unsupported,
}

/// Explicit semantic/static HTML output for one typed semantic node.
#[derive(Clone, Debug)]
pub enum SemanticHtmlPolicy {
    /// Emit through a validated structural DOM plan.
    Dom(NodeDomSpec),
    /// Semantic HTML export is intentionally unsupported.
    Unsupported,
}

impl SemanticHtmlPolicy {
    /// Construct a supported semantic HTML policy.
    pub fn dom(spec: NodeDomSpec) -> Self {
        Self::Dom(spec)
    }
}

/// A closed, deterministic plain-text/accessibility projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextProjection {
    /// Emit a fixed string.
    Static(String),
    /// Emit one declared string attribute with an optional prefix/suffix.
    Attr {
        source: String,
        prefix: String,
        suffix: String,
    },
    /// Emit one declared boolean attribute using closed true/false strings.
    BoolAttr {
        source: String,
        when_true: String,
        when_false: String,
    },
    /// Emit semantic child projections joined by `separator`.
    Content { separator: String },
    /// Concatenate closed projections in order.
    Sequence(Vec<TextProjection>),
}

impl TextProjection {
    /// Project a fixed string.
    pub fn text(value: impl Into<String>) -> Self {
        Self::Static(value.into())
    }

    /// Project one declared string attribute.
    pub fn attr(source: impl Into<String>) -> Self {
        Self::Attr {
            source: source.into(),
            prefix: String::new(),
            suffix: String::new(),
        }
    }

    /// Prefix this attribute projection. Other projection kinds are returned
    /// unchanged so builder-style call sites remain explicit and predictable.
    pub fn prefixed(mut self, prefix: impl Into<String>) -> Self {
        if let Self::Attr {
            prefix: current, ..
        } = &mut self
        {
            *current = prefix.into();
        }
        self
    }

    /// Suffix this attribute projection.
    pub fn suffixed(mut self, suffix: impl Into<String>) -> Self {
        if let Self::Attr {
            suffix: current, ..
        } = &mut self
        {
            *current = suffix.into();
        }
        self
    }

    /// Project one declared boolean attribute.
    pub fn boolean(
        source: impl Into<String>,
        when_true: impl Into<String>,
        when_false: impl Into<String>,
    ) -> Self {
        Self::BoolAttr {
            source: source.into(),
            when_true: when_true.into(),
            when_false: when_false.into(),
        }
    }

    /// Project semantic children without a separator.
    pub fn content() -> Self {
        Self::Content {
            separator: String::new(),
        }
    }

    /// Project semantic children separated by a fixed string.
    pub fn content_separated(separator: impl Into<String>) -> Self {
        Self::Content {
            separator: separator.into(),
        }
    }

    /// Concatenate several projections.
    pub fn sequence(parts: impl IntoIterator<Item = TextProjection>) -> Self {
        Self::Sequence(parts.into_iter().collect())
    }

    pub(crate) fn validate(
        &self,
        attr_keys: &[&str],
        atom: bool,
    ) -> Result<(), SerializationPolicyError> {
        fn walk(
            projection: &TextProjection,
            attr_keys: &[&str],
            has_intrinsic: &mut bool,
        ) -> Result<(), SerializationPolicyError> {
            match projection {
                TextProjection::Static(value) => {
                    *has_intrinsic |= !value.is_empty();
                }
                TextProjection::Attr { source, .. } | TextProjection::BoolAttr { source, .. } => {
                    if !attr_keys.contains(&source.as_str()) {
                        return Err(SerializationPolicyError::UnknownTextAttr {
                            source: source.clone(),
                        });
                    }
                    *has_intrinsic = true;
                }
                TextProjection::Content { .. } => {}
                TextProjection::Sequence(parts) => {
                    for part in parts {
                        walk(part, attr_keys, has_intrinsic)?;
                    }
                }
            }
            Ok(())
        }

        let mut has_intrinsic = false;
        walk(self, attr_keys, &mut has_intrinsic)?;
        if atom && !has_intrinsic {
            return Err(SerializationPolicyError::EmptyAtomicText);
        }
        Ok(())
    }
}

/// Explicit plain-text/accessibility support.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlainTextPolicy {
    /// Emit through the closed projection.
    Projection(TextProjection),
    /// Plain-text/accessibility export is intentionally unsupported.
    Unsupported,
}

impl PlainTextPolicy {
    /// Construct a supported projection policy.
    pub fn projected(projection: TextProjection) -> Self {
        Self::Projection(projection)
    }
}

/// Explicit private-clipboard behavior.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardPolicy {
    /// Preserve the validated semantic node JSON and its wire version.
    Semantic,
    /// This node cannot cross the clipboard boundary.
    Unsupported,
}

/// Complete output contract for one exact typed semantic marker.
#[derive(Clone, Debug)]
pub struct NodeSerializationSpec {
    semantic_type_id: TypeId,
    semantic_rust_type: &'static str,
    node_type: &'static str,
    markdown: Option<MarkdownPolicy>,
    html: Option<SemanticHtmlPolicy>,
    plain_text: Option<PlainTextPolicy>,
    clipboard: Option<ClipboardPolicy>,
}

impl NodeSerializationSpec {
    /// Begin a policy declaration for the exact semantic marker `N`.
    pub fn for_node<N: RichTextNodeType>() -> Self {
        Self {
            semantic_type_id: TypeId::of::<N>(),
            semantic_rust_type: std::any::type_name::<N>(),
            node_type: N::NAME,
            markdown: None,
            html: None,
            plain_text: None,
            clipboard: None,
        }
    }

    /// Declare Markdown support or explicit non-support.
    pub fn markdown(mut self, policy: MarkdownPolicy) -> Self {
        self.markdown = Some(policy);
        self
    }

    /// Declare semantic HTML support or explicit non-support.
    pub fn html(mut self, policy: SemanticHtmlPolicy) -> Self {
        self.html = Some(policy);
        self
    }

    /// Declare plain-text/accessibility support or explicit non-support.
    pub fn plain_text(mut self, policy: PlainTextPolicy) -> Self {
        self.plain_text = Some(policy);
        self
    }

    /// Declare clipboard support or explicit non-support.
    pub fn clipboard(mut self, policy: ClipboardPolicy) -> Self {
        self.clipboard = Some(policy);
        self
    }

    /// Exact Rust semantic marker identity.
    pub fn semantic_type_id(&self) -> TypeId {
        self.semantic_type_id
    }

    /// Rust marker name used only in diagnostics.
    pub fn semantic_rust_type(&self) -> &'static str {
        self.semantic_rust_type
    }

    /// Stable persisted node name.
    pub fn node_type(&self) -> &'static str {
        self.node_type
    }

    /// Declared Markdown policy, if the builder declaration is complete.
    pub fn markdown_policy(&self) -> Option<MarkdownPolicy> {
        self.markdown
    }

    /// Declared HTML policy, if complete.
    pub fn html_policy(&self) -> Option<&SemanticHtmlPolicy> {
        self.html.as_ref()
    }

    /// Declared plain-text policy, if complete.
    pub fn plain_text_policy(&self) -> Option<&PlainTextPolicy> {
        self.plain_text.as_ref()
    }

    /// Declared clipboard policy, if complete.
    pub fn clipboard_policy(&self) -> Option<ClipboardPolicy> {
        self.clipboard
    }

    pub(crate) fn missing_policies(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.markdown.is_none() {
            missing.push("markdown");
        }
        if self.html.is_none() {
            missing.push("semantic_html");
        }
        if self.plain_text.is_none() {
            missing.push("plain_text");
        }
        if self.clipboard.is_none() {
            missing.push("clipboard");
        }
        missing
    }
}

/// A complete model-derived clipboard payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardExport {
    /// Validated Pine slice JSON for the private MIME type.
    pub json: String,
    /// Sanitized semantic HTML, independent of component DOM.
    pub html: String,
    /// Deterministic plain text/accessibility fallback.
    pub plain_text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSlice {
    content: Vec<WireNode>,
    #[serde(rename = "openStart", default)]
    open_start: usize,
    #[serde(rename = "openEnd", default)]
    open_end: usize,
}

/// Policy or export failure. Exporters fail closed instead of silently
/// dropping an unsupported typed atom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerializationError {
    /// The document/slice is not valid for this runtime schema.
    InvalidModel(String),
    /// A typed node explicitly does not support this output.
    Unsupported {
        node_type: String,
        format: &'static str,
    },
    /// A structural HTML projection rejected attrs or values.
    InvalidSemanticHtml { node_type: String, error: String },
    /// A plain-text projection received the wrong attr value shape.
    InvalidTextProjection {
        node_type: String,
        source: String,
        expected: &'static str,
    },
    /// A built-in URL was outside Pine's safe export allowlist.
    UnsafeUrl(String),
    /// Clipboard JSON syntax or encoding failed.
    ClipboardJson(String),
    /// Clipboard open depths exceed the supplied slice tree.
    InvalidClipboardOpenDepth {
        side: &'static str,
        depth: usize,
        maximum: usize,
    },
}

impl fmt::Display for SerializationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModel(error) => write!(formatter, "invalid semantic model: {error}"),
            Self::Unsupported { node_type, format } => {
                write!(
                    formatter,
                    "typed node `{node_type}` does not support {format} export"
                )
            }
            Self::InvalidSemanticHtml { node_type, error } => {
                write!(formatter, "semantic HTML for `{node_type}` failed: {error}")
            }
            Self::InvalidTextProjection {
                node_type,
                source,
                expected,
            } => write!(
                formatter,
                "plain-text projection for `{node_type}` expected attr `{source}` to be {expected}"
            ),
            Self::UnsafeUrl(url) => write!(formatter, "unsafe URL `{url}` in semantic HTML"),
            Self::ClipboardJson(error) => write!(formatter, "invalid Pine clipboard JSON: {error}"),
            Self::InvalidClipboardOpenDepth {
                side,
                depth,
                maximum,
            } => write!(
                formatter,
                "clipboard {side} open depth {depth} exceeds slice depth {maximum}"
            ),
        }
    }
}

impl std::error::Error for SerializationError {}

/// Builder-time validation failure for a policy's closed projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SerializationPolicyError {
    UnknownTextAttr {
        source: String,
    },
    EmptyAtomicText,
    HtmlTypeMismatch {
        policy_rust_type: String,
        html_rust_type: String,
    },
    InvalidHtml(String),
}

impl fmt::Display for SerializationPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTextAttr { source } => {
                write!(
                    formatter,
                    "plain-text projection uses unknown attr `{source}`"
                )
            }
            Self::EmptyAtomicText => formatter.write_str(
                "atomic plain-text projection must emit an attr or non-empty static text",
            ),
            Self::HtmlTypeMismatch {
                policy_rust_type,
                html_rust_type,
            } => write!(
                formatter,
                "semantic HTML uses Rust marker `{html_rust_type}`, policy belongs to `{policy_rust_type}`"
            ),
            Self::InvalidHtml(error) => write!(formatter, "invalid semantic HTML policy: {error}"),
        }
    }
}

impl NodeSerializationSpec {
    pub(crate) fn validate(
        &self,
        attr_keys: &[&str],
        atom: bool,
    ) -> Result<(), SerializationPolicyError> {
        if let Some(SemanticHtmlPolicy::Dom(html)) = &self.html {
            if html.semantic_type_id() != self.semantic_type_id {
                return Err(SerializationPolicyError::HtmlTypeMismatch {
                    policy_rust_type: self.semantic_rust_type.to_string(),
                    html_rust_type: html.semantic_rust_type().to_string(),
                });
            }
            html.validate(attr_keys, atom)
                .map_err(|error| SerializationPolicyError::InvalidHtml(error.to_string()))?;
        }
        if let Some(PlainTextPolicy::Projection(projection)) = &self.plain_text {
            projection.validate(attr_keys, atom)?;
        }
        Ok(())
    }
}

impl EditorRuntime {
    /// Export sanitized semantic HTML from the model tree.
    pub fn export_semantic_html(&self, doc: &Node) -> Result<String, SerializationError> {
        self.schema()
            .check_node(doc)
            .map_err(|error| SerializationError::InvalidModel(error.to_string()))?;
        HtmlExporter::new(self).compile_document(doc)
    }

    /// Export deterministic plain text/accessibility content from the model.
    pub fn export_plain_text(&self, doc: &Node) -> Result<String, SerializationError> {
        self.schema()
            .check_node(doc)
            .map_err(|error| SerializationError::InvalidModel(error.to_string()))?;
        PlainTextExporter::new(self).node(doc)
    }

    /// Export Markdown while honoring explicit typed-node support policies.
    pub fn export_markdown(&self, doc: &Node) -> Result<String, SerializationError> {
        self.schema()
            .check_node(doc)
            .map_err(|error| SerializationError::InvalidModel(error.to_string()))?;
        self.ensure_format_supported(doc, "Markdown", |spec| {
            spec.markdown_policy() == Some(MarkdownPolicy::Supported)
        })?;
        self.markdown_serializer()
            .serialize(doc)
            .map_err(|error| SerializationError::InvalidModel(error.to_string()))
    }

    /// Export private JSON plus sanitized HTML and plain text for a slice.
    pub fn export_clipboard(&self, slice: &Slice) -> Result<ClipboardExport, SerializationError> {
        self.validate_clipboard_nodes(slice.content.as_slice())?;
        validate_open_depths(slice)?;
        let json = serde_json::to_string(slice)
            .map_err(|error| SerializationError::ClipboardJson(error.to_string()))?;
        let html = HtmlExporter::new(self).compile_fragment(&slice.content)?;
        let plain_text = PlainTextExporter::new(self).fragment(&slice.content, "\n\n")?;
        Ok(ClipboardExport {
            json,
            html,
            plain_text,
        })
    }

    /// Decode, migrate, and schema-validate a private Pine clipboard slice.
    pub fn import_clipboard_json(&self, value: &str) -> Result<Slice, SerializationError> {
        let wire = serde_json::from_str::<WireSlice>(value)
            .map_err(|error| SerializationError::ClipboardJson(error.to_string()))?;
        let mut nodes = Vec::with_capacity(wire.content.len());
        for (index, node) in wire.content.into_iter().enumerate() {
            nodes.push(self.schema().materialize_wire_node(node).map_err(|error| {
                SerializationError::ClipboardJson(format!("content[{index}]: {error}"))
            })?);
        }
        let slice = Slice::new(Fragment::from(nodes), wire.open_start, wire.open_end);
        self.validate_clipboard_nodes(slice.content.as_slice())?;
        validate_open_depths(&slice)?;
        Ok(slice)
    }

    fn ensure_format_supported(
        &self,
        node: &Node,
        format: &'static str,
        supported: impl Fn(&NodeSerializationSpec) -> bool + Copy,
    ) -> Result<(), SerializationError> {
        if let Some(spec) = self.lookup_node_serialization(node.type_name())
            && !supported(spec)
        {
            return Err(SerializationError::Unsupported {
                node_type: node.type_name().to_string(),
                format,
            });
        }
        for child in node.content().iter() {
            self.ensure_format_supported(child, format, supported)?;
        }
        Ok(())
    }

    fn validate_clipboard_nodes(&self, nodes: &[Node]) -> Result<(), SerializationError> {
        for node in nodes {
            self.schema()
                .check_node(node)
                .map_err(|error| SerializationError::InvalidModel(error.to_string()))?;
            self.ensure_format_supported(node, "clipboard", |spec| {
                spec.clipboard_policy() == Some(ClipboardPolicy::Semantic)
            })?;
        }
        Ok(())
    }
}

struct HtmlExporter<'a> {
    runtime: &'a EditorRuntime,
}

impl<'a> HtmlExporter<'a> {
    fn new(runtime: &'a EditorRuntime) -> Self {
        Self { runtime }
    }

    fn compile_document(&self, doc: &Node) -> Result<String, SerializationError> {
        self.compile_fragment(doc.content())
    }

    fn compile_fragment(&self, fragment: &Fragment) -> Result<String, SerializationError> {
        let plan = DomFragmentSpec::new().extend(self.nodes(fragment)?);
        let mut output = String::new();
        plan.compile_into(&mut output)
            .map_err(|error| semantic_dom_error("doc", error))?;
        Ok(output)
    }

    fn nodes(&self, fragment: &Fragment) -> Result<Vec<DomNodeSpec>, SerializationError> {
        fragment.iter().map(|child| self.node(child)).collect()
    }

    fn node(&self, node: &Node) -> Result<DomNodeSpec, SerializationError> {
        if node.is_text() {
            return self.text(node);
        }
        if let Some(spec) = self.runtime.lookup_node_serialization(node.type_name()) {
            return match spec.html_policy().expect("runtime validates completeness") {
                SemanticHtmlPolicy::Unsupported => Err(SerializationError::Unsupported {
                    node_type: node.type_name().to_string(),
                    format: "semantic HTML",
                }),
                SemanticHtmlPolicy::Dom(dom) => {
                    let content = self.nodes(node.content())?;
                    dom.semantic_node(node, &content).map_err(|error| {
                        SerializationError::InvalidSemanticHtml {
                            node_type: node.type_name().to_string(),
                            error: error.to_string(),
                        }
                    })
                }
            };
        }

        let content = || self.nodes(node.content());
        match node.type_name() {
            "paragraph" => structural_element(node.type_name(), "p", content()?),
            "blockquote" => structural_element(node.type_name(), "blockquote", content()?),
            "bullet_list" => structural_element(node.type_name(), "ul", content()?),
            "task_list" => {
                let children = content()?;
                let element = DomElementSpec::element("ul")
                    .and_then(|element| element.attr("class", "task-list"))
                    .and_then(|element| element.extend_nodes(children))
                    .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                Ok(element.into())
            }
            "ordered_list" => {
                let mut element = DomElementSpec::element("ol")
                    .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                if let Some(start) = node
                    .attrs()
                    .get("order")
                    .and_then(Value::as_i64)
                    .filter(|value| *value != 1)
                {
                    element = element
                        .attr("start", start.to_string())
                        .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                }
                element = element
                    .extend_nodes(content()?)
                    .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                Ok(element.into())
            }
            "list_item" => structural_element(node.type_name(), "li", content()?),
            "heading" => {
                let level = node
                    .attrs()
                    .get("level")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 6);
                let tag = ["h1", "h2", "h3", "h4", "h5", "h6"][level as usize - 1];
                structural_element(node.type_name(), tag, content()?)
            }
            "code_block" => {
                let code = DomElementSpec::element("code")
                    .and_then(|element| element.text(node.text_content()))
                    .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                let pre = DomElementSpec::element("pre")
                    .and_then(|element| element.child(code))
                    .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                Ok(pre.into())
            }
            "horizontal_rule" => DomElementSpec::element("hr")
                .map(Into::into)
                .map_err(|error| semantic_dom_error(node.type_name(), error)),
            "hard_break" => DomElementSpec::element("br")
                .map(Into::into)
                .map_err(|error| semantic_dom_error(node.type_name(), error)),
            "image" => {
                let src = node
                    .attrs()
                    .get("src")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                validate_builtin_url(src, &["http", "https"], true)?;
                let alt = node
                    .attrs()
                    .get("alt")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let mut image = DomElementSpec::element("img")
                    .and_then(|element| element.attr("src", src))
                    .and_then(|element| element.attr("alt", alt))
                    .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                if let Some(title) = node.attrs().get("title").and_then(Value::as_str) {
                    image = image
                        .attr("title", title)
                        .map_err(|error| semantic_dom_error(node.type_name(), error))?;
                }
                Ok(image.into())
            }
            other => Err(SerializationError::Unsupported {
                node_type: other.to_string(),
                format: "semantic HTML",
            }),
        }
    }

    fn text(&self, node: &Node) -> Result<DomNodeSpec, SerializationError> {
        let mut output = DomNodeSpec::text(node.text().unwrap_or(""));
        for mark in node.marks().iter().rev() {
            let mut wrapper = match mark.type_name() {
                "em" => DomElementSpec::element("em"),
                "strong" => DomElementSpec::element("strong"),
                "code" => DomElementSpec::element("code"),
                "link" => {
                    let href = mark
                        .attrs()
                        .get("href")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    validate_builtin_url(href, &["http", "https", "mailto"], true)?;
                    let mut link =
                        DomElementSpec::element("a").and_then(|element| element.attr("href", href));
                    if let Some(title) = mark.attrs().get("title").and_then(Value::as_str) {
                        link = link.and_then(|element| element.attr("title", title));
                    }
                    link
                }
                other => DomElementSpec::element("span")
                    .and_then(|element| element.attr("data-mark", other)),
            }
            .map_err(|error| semantic_dom_error(node.type_name(), error))?;
            wrapper = wrapper
                .node(output)
                .map_err(|error| semantic_dom_error(node.type_name(), error))?;
            output = wrapper.into();
        }
        Ok(output)
    }
}

struct PlainTextExporter<'a> {
    runtime: &'a EditorRuntime,
}

impl<'a> PlainTextExporter<'a> {
    fn new(runtime: &'a EditorRuntime) -> Self {
        Self { runtime }
    }

    fn fragment(&self, fragment: &Fragment, separator: &str) -> Result<String, SerializationError> {
        fragment
            .iter()
            .map(|child| self.node(child))
            .collect::<Result<Vec<_>, _>>()
            .map(|parts| parts.join(separator))
    }

    fn node(&self, node: &Node) -> Result<String, SerializationError> {
        if node.is_text() {
            return Ok(node.text().unwrap_or("").to_string());
        }
        if let Some(spec) = self.runtime.lookup_node_serialization(node.type_name()) {
            return match spec
                .plain_text_policy()
                .expect("runtime validates completeness")
            {
                PlainTextPolicy::Unsupported => Err(SerializationError::Unsupported {
                    node_type: node.type_name().to_string(),
                    format: "plain text",
                }),
                PlainTextPolicy::Projection(projection) => self.project(node, projection),
            };
        }
        match node.type_name() {
            "doc" => self.fragment(node.content(), "\n\n"),
            "paragraph" | "heading" | "code_block" => self.fragment(node.content(), ""),
            "blockquote" => self.fragment(node.content(), "\n"),
            "bullet_list" | "ordered_list" | "task_list" => self.fragment(node.content(), "\n"),
            "list_item" => self.fragment(node.content(), "\n"),
            "hard_break" => Ok("\n".to_string()),
            "horizontal_rule" => Ok("---".to_string()),
            "image" => Ok(node
                .attrs()
                .get("alt")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()),
            _ if node.child_count() > 0 => self.fragment(node.content(), ""),
            other => Err(SerializationError::Unsupported {
                node_type: other.to_string(),
                format: "plain text",
            }),
        }
    }

    fn project(
        &self,
        node: &Node,
        projection: &TextProjection,
    ) -> Result<String, SerializationError> {
        match projection {
            TextProjection::Static(value) => Ok(value.clone()),
            TextProjection::Attr {
                source,
                prefix,
                suffix,
            } => {
                let value = node
                    .attrs()
                    .get(source)
                    .and_then(Value::as_str)
                    .ok_or_else(|| SerializationError::InvalidTextProjection {
                        node_type: node.type_name().to_string(),
                        source: source.clone(),
                        expected: "a string",
                    })?;
                Ok(format!("{prefix}{value}{suffix}"))
            }
            TextProjection::BoolAttr {
                source,
                when_true,
                when_false,
            } => {
                let value = node
                    .attrs()
                    .get(source)
                    .and_then(Value::as_bool)
                    .ok_or_else(|| SerializationError::InvalidTextProjection {
                        node_type: node.type_name().to_string(),
                        source: source.clone(),
                        expected: "a boolean",
                    })?;
                Ok(if value { when_true } else { when_false }.clone())
            }
            TextProjection::Content { separator } => self.fragment(node.content(), separator),
            TextProjection::Sequence(parts) => parts
                .iter()
                .map(|part| self.project(node, part))
                .collect::<Result<Vec<_>, _>>()
                .map(|parts| parts.concat()),
        }
    }
}

fn structural_element(
    node_type: &str,
    tag: &str,
    children: Vec<DomNodeSpec>,
) -> Result<DomNodeSpec, SerializationError> {
    DomElementSpec::element(tag)
        .and_then(|element| element.extend_nodes(children))
        .map(Into::into)
        .map_err(|error| semantic_dom_error(node_type, error))
}

fn semantic_dom_error(node_type: &str, error: DomOutputError) -> SerializationError {
    SerializationError::InvalidSemanticHtml {
        node_type: node_type.to_string(),
        error: error.to_string(),
    }
}

fn validate_builtin_url(
    value: &str,
    allowed_schemes: &[&str],
    allow_relative: bool,
) -> Result<(), SerializationError> {
    if value.chars().any(char::is_control) {
        return Err(SerializationError::UnsafeUrl(value.to_string()));
    }
    let prefix = value.split(['/', '?', '#']).next().unwrap_or_default();
    if let Some((scheme, _)) = prefix.split_once(':') {
        let valid = !scheme.is_empty()
            && scheme.chars().enumerate().all(|(index, ch)| {
                ch.is_ascii_alphabetic()
                    || (index > 0 && (ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.')))
            })
            && allowed_schemes
                .iter()
                .any(|allowed| scheme.eq_ignore_ascii_case(allowed));
        if !valid {
            return Err(SerializationError::UnsafeUrl(value.to_string()));
        }
    } else if !allow_relative {
        return Err(SerializationError::UnsafeUrl(value.to_string()));
    }
    Ok(())
}

fn validate_open_depths(slice: &Slice) -> Result<(), SerializationError> {
    let start_max = slice
        .content
        .child(0)
        .map_or(0, |node| boundary_depth(node, true));
    let end_max = slice
        .content
        .child(slice.content.len().saturating_sub(1))
        .map_or(0, |node| boundary_depth(node, false));
    if slice.open_start > start_max {
        return Err(SerializationError::InvalidClipboardOpenDepth {
            side: "start",
            depth: slice.open_start,
            maximum: start_max,
        });
    }
    if slice.open_end > end_max {
        return Err(SerializationError::InvalidClipboardOpenDepth {
            side: "end",
            depth: slice.open_end,
            maximum: end_max,
        });
    }
    Ok(())
}

fn boundary_depth(node: &Node, start: bool) -> usize {
    let mut depth = 0;
    let mut current = node;
    while current.child_count() > 0 {
        let index = if start { 0 } else { current.child_count() - 1 };
        let Some(child) = current.child(index) else {
            break;
        };
        depth += 1;
        current = child;
    }
    depth
}
