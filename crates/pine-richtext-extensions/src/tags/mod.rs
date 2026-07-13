//! Typed inline tags/chips.
//!
//! A tag is semantic editor data, not a serialized component. Its closed wire
//! shape contains an external id, a human label, and an optional visual kind.
//! JSON and Pine's private clipboard MIME preserve that full shape; plain text
//! exports `#label` and Markdown exports the CommonMark-safe `\#label`.

use std::fmt;
use std::sync::Arc;

use pine_richtext::commands::BoxedCommand;
use pine_richtext::extension::{NamedCommand, RichTextExtension};
use pine_richtext::markdown::pulldown_cmark::Event;
use pine_richtext::model::{Attrs, Fragment, Node, NodeSpec};
use pine_richtext::render::{DomAttrBinding, DomOutputSpec, NodeDomSpec};
use pine_richtext::serialization::{
    ClipboardPolicy, MarkdownPolicy, NodeSerializationSpec, PlainTextPolicy, SemanticHtmlPolicy,
    TextProjection,
};
use pine_richtext::state::{EditorState, Selection};
use pine_richtext::{RichTextNodeAttrs, RichTextNodeType, TypedNodeSpec};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

mod dom;

#[cfg(feature = "view")]
mod view;

#[cfg(feature = "view")]
pub use view::PineRichTextTag;

/// Lossless clipboard flavor for one tag atom.
pub const TAG_CLIPBOARD_MIME: &str = "application/x-pine-richtext-tag+json";

const MAX_TAG_ID_CHARS: usize = 256;
const MAX_TAG_LABEL_CHARS: usize = 256;

/// Validated external identity for a tag.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TagId(String);

impl TagId {
    pub fn new(value: impl Into<String>) -> Result<Self, TagValueError> {
        let value = value.into();
        validate_tag_text("id", &value, MAX_TAG_ID_CHARS, false)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TagId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for TagId {
    type Error = TagValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for TagId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TagId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Validated user-visible tag label.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TagLabel(String);

impl TagLabel {
    pub fn new(value: impl Into<String>) -> Result<Self, TagValueError> {
        let value = value.into();
        validate_tag_text("label", &value, MAX_TAG_LABEL_CHARS, true)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TagLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for TagLabel {
    type Error = TagValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for TagLabel {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for TagLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

fn validate_tag_text(
    field: &'static str,
    value: &str,
    max_chars: usize,
    reject_edge_whitespace: bool,
) -> Result<(), TagValueError> {
    let length = value.chars().count();
    if value.is_empty() {
        return Err(TagValueError::Empty { field });
    }
    if length > max_chars {
        return Err(TagValueError::TooLong {
            field,
            max_chars,
            actual_chars: length,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(TagValueError::ControlCharacter { field });
    }
    if reject_edge_whitespace && value.trim() != value {
        return Err(TagValueError::EdgeWhitespace { field });
    }
    if !reject_edge_whitespace && value.chars().any(char::is_whitespace) {
        return Err(TagValueError::Whitespace { field });
    }
    Ok(())
}

/// Invalid semantic tag data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagValueError {
    Empty {
        field: &'static str,
    },
    TooLong {
        field: &'static str,
        max_chars: usize,
        actual_chars: usize,
    },
    ControlCharacter {
        field: &'static str,
    },
    Whitespace {
        field: &'static str,
    },
    EdgeWhitespace {
        field: &'static str,
    },
}

impl fmt::Display for TagValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "tag {field} must not be empty"),
            Self::TooLong {
                field,
                max_chars,
                actual_chars,
            } => write!(
                formatter,
                "tag {field} has {actual_chars} characters; maximum is {max_chars}"
            ),
            Self::ControlCharacter { field } => {
                write!(formatter, "tag {field} must not contain control characters")
            }
            Self::Whitespace { field } => {
                write!(formatter, "tag {field} must not contain whitespace")
            }
            Self::EdgeWhitespace { field } => {
                write!(
                    formatter,
                    "tag {field} must not start or end with whitespace"
                )
            }
        }
    }
}

impl std::error::Error for TagValueError {}

/// Closed, validated visual vocabulary. Applications customize each kind with
/// CSS variables; arbitrary CSS is never persisted in the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagKind {
    Neutral,
    Info,
    Success,
    Warning,
    Danger,
}

impl TagKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Info => "info",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Danger => "danger",
        }
    }
}

/// Complete persisted attrs for [`TagNode`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, RichTextNodeAttrs)]
pub struct TagAttrs {
    pub id: TagId,
    pub label: TagLabel,
    #[serde(default)]
    pub kind: Option<TagKind>,
}

impl TagAttrs {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Result<Self, TagValueError> {
        Ok(Self {
            id: TagId::new(id)?,
            label: TagLabel::new(label)?,
            kind: None,
        })
    }

    pub fn with_kind(mut self, kind: TagKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Deterministic lossy text representation used by plain-text extraction.
    pub fn plain_text(&self) -> String {
        format!("#{}", self.label)
    }

    /// Deterministic CommonMark-safe semantic fallback. Markdown deliberately
    /// preserves the label, not the external id or UI kind.
    pub fn markdown(&self) -> String {
        format!("\\#{}", self.label)
    }

    pub fn to_attrs(&self) -> Result<Attrs, serde_json::Error> {
        attrs_from_value(serde_json::to_value(self)?)
    }
}

/// Typed inline atom marker.
pub struct TagNode;

impl RichTextNodeType for TagNode {
    const NAME: &'static str = "tag";
    const VERSION: u32 = 1;
    type Attrs = TagAttrs;

    fn spec() -> NodeSpec {
        NodeSpec::new(Self::NAME)
            .group("inline")
            .inline()
            .atom()
            .required_attr("id")
            .required_attr("label")
            .attr("kind", Value::Null)
            .leaf_text(tag_leaf_text)
    }
}

fn tag_leaf_text(node: &Node) -> String {
    decode_attrs(node)
        .map(|attrs| attrs.plain_text())
        .unwrap_or_else(|| "#tag".to_string())
}

/// Lossless tag-only clipboard payload. Pine's normal JSON slice remains the
/// canonical multi-node clipboard format; this flavor is convenient for tag
/// pickers and integrations that copy one chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagClipboardPayload {
    #[serde(rename = "type")]
    node_type: String,
    version: u32,
    pub attrs: TagAttrs,
}

impl TagClipboardPayload {
    pub fn new(attrs: TagAttrs) -> Self {
        Self {
            node_type: TagNode::NAME.to_string(),
            version: TagNode::VERSION,
            attrs,
        }
    }

    pub fn node_type(&self) -> &str {
        &self.node_type
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn plain_text(&self) -> String {
        self.attrs.plain_text()
    }

    pub fn markdown(&self) -> String {
        self.attrs.markdown()
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(value: &str) -> Result<Self, serde_json::Error> {
        let payload = serde_json::from_str::<Self>(value)?;
        if payload.node_type != TagNode::NAME {
            return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "tag clipboard expected type `{}`, found `{}`",
                TagNode::NAME,
                payload.node_type
            )));
        }
        if payload.version != TagNode::VERSION {
            return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "tag clipboard expected version {}, found {}",
                TagNode::VERSION,
                payload.version
            )));
        }
        Ok(payload)
    }
}

/// Model and (with `view`) typed chip-view extension.
#[derive(Clone, Copy, Debug, Default)]
pub struct TagsExtension;

impl RichTextExtension for TagsExtension {
    fn name(&self) -> &str {
        "tags"
    }

    fn typed_nodes(&self) -> Vec<TypedNodeSpec> {
        vec![TypedNodeSpec::of::<TagNode>()]
    }

    fn dom_views(&self) -> Vec<pine_richtext::render::NodeDomSpec> {
        dom::dom_specs()
    }

    fn node_serialization(&self) -> Vec<NodeSerializationSpec> {
        vec![
            NodeSerializationSpec::for_node::<TagNode>()
                .markdown(MarkdownPolicy::Supported)
                .html(SemanticHtmlPolicy::dom(NodeDomSpec::nested::<TagNode>(
                    DomOutputSpec::element("span")
                        .attr("data-pine-tag", "true")
                        .attr("role", "group")
                        .attr("aria-roledescription", "tag")
                        .bind_attr(DomAttrBinding::string("data-tag-id", "id"))
                        .bind_attr(DomAttrBinding::string("aria-label", "label"))
                        .bind_attr(DomAttrBinding::token(
                            "data-kind",
                            "kind",
                            ["neutral", "info", "success", "warning", "danger"],
                        ))
                        .text("#")
                        .bind_text("label"),
                )))
                .plain_text(PlainTextPolicy::projected(
                    TextProjection::attr("label").prefixed("#"),
                ))
                .clipboard(ClipboardPolicy::Semantic),
        ]
    }

    fn commands(&self) -> Vec<(String, NamedCommand)> {
        named_commands()
    }

    fn markdown_node_emitters(&self) -> Vec<(String, pine_richtext::markdown::NodeEmitter)> {
        let emitter = Arc::new(
            |node: &Node,
             _parent: &Node,
             _index: usize,
             sink: &mut pine_richtext::markdown::EventSink<'_>| {
                if let Some(attrs) = decode_attrs(node) {
                    sink.push(Event::Text(attrs.plain_text().into()));
                }
            },
        );
        vec![(TagNode::NAME.to_string(), emitter)]
    }
}

/// Replace the current selection with one tag atom.
pub fn insert_tag(attrs: TagAttrs) -> BoxedCommand {
    insert_tag_in_range(attrs, None)
}

/// Replace an explicit text range. Suggestion UIs use this to consume their
/// trigger/query even if focus temporarily moved to the picker.
pub fn insert_tag_over(from: usize, to: usize, attrs: TagAttrs) -> BoxedCommand {
    insert_tag_in_range(attrs, Some((from, to)))
}

fn insert_tag_in_range(attrs: TagAttrs, range: Option<(usize, usize)>) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let node = build_tag(state, &attrs)?;
        let mut transaction = state.tr();
        if let Some((from, to)) = range {
            if from > to || to > state.doc().content_size() {
                return None;
            }
            transaction
                .set_selection(Selection::text_between(from, to))
                .ok()?;
        }
        transaction.replace_selection_with(node).ok()?;
        Some(transaction)
    })
}

/// Update the selected or caret-adjacent tag without changing its marks.
pub fn update_tag(attrs: TagAttrs) -> BoxedCommand {
    update_tag_target(None, attrs)
}

pub fn update_tag_at(position: usize, attrs: TagAttrs) -> BoxedCommand {
    update_tag_target(Some(position), attrs)
}

fn update_tag_target(position: Option<usize>, attrs: TagAttrs) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let position = position.or_else(|| current_tag_position(state))?;
        let source = state.doc().node_at(position).ok()??;
        if source.type_name() != TagNode::NAME {
            return None;
        }
        let replacement = build_tag(state, &attrs)?.with_marks(source.marks().to_vec());
        if replacement == *source {
            return None;
        }
        let mut transaction = state.tr();
        transaction
            .replace_with(
                position,
                position + source.node_size(),
                Fragment::from(replacement),
            )
            .ok()?;
        if matches!(state.selection(), Selection::Node { anchor } if *anchor == position) {
            transaction.set_selection(Selection::node(position)).ok()?;
        }
        Some(transaction)
    })
}

/// Delete the selected or caret-adjacent tag atom.
pub fn delete_tag() -> BoxedCommand {
    delete_tag_target(None)
}

pub fn delete_tag_at(position: usize) -> BoxedCommand {
    delete_tag_target(Some(position))
}

fn delete_tag_target(position: Option<usize>) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let position = position.or_else(|| current_tag_position(state))?;
        let node = state.doc().node_at(position).ok()??;
        if node.type_name() != TagNode::NAME {
            return None;
        }
        let mut transaction = state.tr();
        transaction.set_selection(Selection::text(position)).ok()?;
        transaction
            .delete(position, position + node.node_size())
            .ok()?;
        Some(transaction)
    })
}

/// Select the caret-adjacent tag atom as one node.
pub fn select_tag() -> BoxedCommand {
    select_tag_target(None)
}

pub fn select_tag_at(position: usize) -> BoxedCommand {
    select_tag_target(Some(position))
}

fn select_tag_target(position: Option<usize>) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let position = position.or_else(|| current_tag_position(state))?;
        let node = state.doc().node_at(position).ok()??;
        if node.type_name() != TagNode::NAME {
            return None;
        }
        let mut transaction = state.tr();
        transaction.set_selection(Selection::node(position)).ok()?;
        Some(transaction)
    })
}

/// Select a tag immediately before a collapsed text caret.
pub fn select_tag_backward() -> BoxedCommand {
    Box::new(|state: &EditorState| {
        let Selection::Text { anchor, head } = state.selection() else {
            return None;
        };
        if anchor != head {
            return None;
        }
        let resolved = state.doc().resolve(*head).ok()?;
        let before = resolved.node_before()?;
        if before.type_name() != TagNode::NAME {
            return None;
        }
        let position = head.saturating_sub(before.node_size());
        let mut transaction = state.tr();
        transaction.set_selection(Selection::node(position)).ok()?;
        Some(transaction)
    })
}

/// Select a tag immediately after a collapsed text caret.
pub fn select_tag_forward() -> BoxedCommand {
    Box::new(|state: &EditorState| {
        let Selection::Text { anchor, head } = state.selection() else {
            return None;
        };
        if anchor != head {
            return None;
        }
        let after = state.doc().resolve(*head).ok()?.node_after()?;
        if after.type_name() != TagNode::NAME {
            return None;
        }
        let mut transaction = state.tr();
        transaction.set_selection(Selection::node(*head)).ok()?;
        Some(transaction)
    })
}

fn leave_selected_tag(direction: i8) -> BoxedCommand {
    Box::new(move |state: &EditorState| {
        let Selection::Node { anchor } = state.selection() else {
            return None;
        };
        let node = state.doc().node_at(*anchor).ok()??;
        if node.type_name() != TagNode::NAME {
            return None;
        }
        let position = if direction < 0 {
            *anchor
        } else {
            anchor + node.node_size()
        };
        let mut transaction = state.tr();
        transaction.set_selection(Selection::text(position)).ok()?;
        Some(transaction)
    })
}

/// Move a selected tag's caret to its left boundary.
pub fn leave_selected_tag_left() -> BoxedCommand {
    leave_selected_tag(-1)
}

/// Move a selected tag's caret to its right boundary.
pub fn leave_selected_tag_right() -> BoxedCommand {
    leave_selected_tag(1)
}

fn build_tag(state: &EditorState, attrs: &TagAttrs) -> Option<Node> {
    state
        .schema()
        .node(TagNode::NAME, attrs.to_attrs().ok()?, Fragment::empty())
        .ok()
}

fn current_tag_position(state: &EditorState) -> Option<usize> {
    match state.selection() {
        Selection::Node { anchor } => state
            .doc()
            .node_at(*anchor)
            .ok()
            .flatten()
            .is_some_and(|node| node.type_name() == TagNode::NAME)
            .then_some(*anchor),
        Selection::Text { anchor, head } if anchor == head => {
            let resolved = state.doc().resolve(*head).ok()?;
            if let Some(before) = resolved.node_before()
                && before.type_name() == TagNode::NAME
            {
                return Some(head.saturating_sub(before.node_size()));
            }
            resolved
                .node_after()
                .is_some_and(|node| node.type_name() == TagNode::NAME)
                .then_some(*head)
        }
        _ => None,
    }
}

#[derive(Deserialize)]
struct InsertTagArgs {
    #[serde(flatten)]
    attrs: TagAttrs,
    #[serde(default)]
    from: Option<usize>,
    #[serde(default)]
    to: Option<usize>,
}

#[derive(Deserialize)]
struct TargetTagArgs {
    #[serde(default)]
    pos: Option<usize>,
}

#[derive(Deserialize)]
struct UpdateTagArgs {
    #[serde(flatten)]
    attrs: TagAttrs,
    #[serde(default)]
    pos: Option<usize>,
}

fn named_commands() -> Vec<(String, NamedCommand)> {
    vec![
        named("insert_tag", |value| {
            let args = serde_json::from_value::<InsertTagArgs>(value).ok()?;
            match (args.from, args.to) {
                (Some(from), Some(to)) => Some(insert_tag_over(from, to, args.attrs)),
                (None, None) => Some(insert_tag(args.attrs)),
                _ => None,
            }
        }),
        named("update_tag", |value| {
            let args = serde_json::from_value::<UpdateTagArgs>(value).ok()?;
            Some(match args.pos {
                Some(position) => update_tag_at(position, args.attrs),
                None => update_tag(args.attrs),
            })
        }),
        named("delete_tag", |value| {
            let args = serde_json::from_value::<TargetTagArgs>(value).ok()?;
            Some(match args.pos {
                Some(position) => delete_tag_at(position),
                None => delete_tag(),
            })
        }),
        named("select_tag", |value| {
            let args = serde_json::from_value::<TargetTagArgs>(value).ok()?;
            Some(match args.pos {
                Some(position) => select_tag_at(position),
                None => select_tag(),
            })
        }),
    ]
}

fn named(
    name: &str,
    factory: impl Fn(Value) -> Option<BoxedCommand> + Send + Sync + 'static,
) -> (String, NamedCommand) {
    (name.to_string(), Arc::new(factory))
}

fn attrs_from_value(value: Value) -> Result<Attrs, serde_json::Error> {
    match value {
        Value::Object(object) => Ok(object.into_iter().collect()),
        other => serde_json::from_value::<Attrs>(other),
    }
}

fn decode_attrs(node: &Node) -> Option<TagAttrs> {
    serde_json::from_value(serde_json::to_value(node.attrs()).ok()?).ok()
}

/// Configurable lightweight suggestion matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagSuggestionConfig {
    pub trigger: char,
    pub require_boundary: bool,
    pub allow_spaces: bool,
    pub minimum_query_chars: usize,
    pub maximum_query_chars: usize,
}

impl Default for TagSuggestionConfig {
    fn default() -> Self {
        Self {
            trigger: '#',
            require_boundary: true,
            allow_spaces: false,
            minimum_query_chars: 0,
            maximum_query_chars: 64,
        }
    }
}

/// Trigger/query range derived from a caret prefix without reading the full
/// document or DOM selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagSuggestionMatch {
    pub trigger: char,
    pub query: String,
    pub from: usize,
    pub to: usize,
}

type QueryCharPredicate = Arc<dyn Fn(char) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct TagSuggestionMatcher {
    config: TagSuggestionConfig,
    accepts: QueryCharPredicate,
}

impl fmt::Debug for TagSuggestionMatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TagSuggestionMatcher")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl TagSuggestionMatcher {
    pub fn new(config: TagSuggestionConfig) -> Self {
        Self {
            config,
            accepts: Arc::new(|character| {
                character.is_alphanumeric() || matches!(character, '_' | '-')
            }),
        }
    }

    pub fn with_query_character(
        mut self,
        accepts: impl Fn(char) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.accepts = Arc::new(accepts);
        self
    }

    pub fn config(&self) -> &TagSuggestionConfig {
        &self.config
    }

    pub fn match_prefix(&self, caret_prefix: &str, caret: usize) -> Option<TagSuggestionMatch> {
        let chars = caret_prefix.chars().collect::<Vec<_>>();
        let trigger_index = chars
            .iter()
            .rposition(|character| *character == self.config.trigger)?;
        if self.config.require_boundary
            && trigger_index > 0
            && (chars[trigger_index - 1].is_alphanumeric()
                || matches!(chars[trigger_index - 1], '_' | '-'))
        {
            return None;
        }

        let query_chars = &chars[trigger_index + 1..];
        if query_chars.len() < self.config.minimum_query_chars
            || query_chars.len() > self.config.maximum_query_chars
            || query_chars.iter().any(|character| {
                !((self.accepts)(*character) || (self.config.allow_spaces && *character == ' '))
            })
        {
            return None;
        }
        let matched_chars = query_chars.len() + 1;
        Some(TagSuggestionMatch {
            trigger: self.config.trigger,
            query: query_chars.iter().collect(),
            from: caret.checked_sub(matched_chars)?,
            to: caret,
        })
    }

    /// Bridge the cheap `Editor::on_change` payload and selection snapshot.
    /// This performs no full-document serialization.
    #[cfg(feature = "view")]
    pub fn match_change(
        &self,
        change: &pine_richtext::view::ChangeInfo,
        snapshot: &pine_richtext::view::SelectionSnapshot,
    ) -> Option<TagSuggestionMatch> {
        if !snapshot.empty || !snapshot.focused || !snapshot.editable {
            return None;
        }
        let Selection::Text { anchor, head } = &snapshot.selection else {
            return None;
        };
        if anchor != head {
            return None;
        }
        self.match_prefix(&change.caret_prefix, *head)
    }
}

impl Default for TagSuggestionMatcher {
    fn default() -> Self {
        Self::new(TagSuggestionConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matcher_uses_character_positions_for_unicode_prefixes() {
        let matched = TagSuggestionMatcher::default()
            .match_prefix("🙂 #rust", 8)
            .unwrap();
        assert_eq!(matched.query, "rust");
        assert_eq!((matched.from, matched.to), (3, 8));
    }

    #[test]
    fn matcher_enforces_boundaries_and_custom_query_characters() {
        let matcher = TagSuggestionMatcher::default();
        assert!(matcher.match_prefix("mail#tag", 8).is_none());
        assert_eq!(matcher.match_prefix("(#tag", 5).unwrap().query, "tag");
        assert!(matcher.match_prefix("#one.two", 8).is_none());

        let matcher = matcher.with_query_character(|character| {
            character.is_alphanumeric() || matches!(character, '_' | '-' | '.')
        });
        assert_eq!(
            matcher.match_prefix("#one.two", 8).unwrap().query,
            "one.two"
        );
    }

    #[test]
    fn clipboard_json_is_lossless_and_text_is_intentionally_label_only() {
        let attrs = TagAttrs::new("tag-42", "Priority")
            .unwrap()
            .with_kind(TagKind::Warning);
        let payload = TagClipboardPayload::new(attrs.clone());
        let encoded = payload.to_json().unwrap();
        assert_eq!(TagClipboardPayload::from_json(&encoded).unwrap(), payload);
        assert_eq!(payload.node_type(), TagNode::NAME);
        assert_eq!(payload.version(), TagNode::VERSION);
        assert!(
            TagClipboardPayload::from_json(&encoded.replace("\"version\":1", "\"version\":2"))
                .is_err()
        );
        assert_eq!(payload.plain_text(), "#Priority");
        assert_eq!(payload.markdown(), "\\#Priority");
        assert_eq!(payload.attrs, attrs);
    }

    #[test]
    fn serde_rejects_invalid_ids_labels_and_kinds() {
        assert!(
            serde_json::from_value::<TagAttrs>(serde_json::json!({
                "id": "has space",
                "label": "Valid",
                "kind": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<TagAttrs>(serde_json::json!({
                "id": "valid",
                "label": " padded ",
                "kind": null
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<TagAttrs>(serde_json::json!({
                "id": "valid",
                "label": "Valid",
                "kind": "rainbow"
            }))
            .is_err()
        );
    }
}
