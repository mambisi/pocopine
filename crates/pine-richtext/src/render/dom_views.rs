//! Validated, extension-owned semantic DOM output.

use std::any::TypeId;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
#[cfg(feature = "view")]
use std::io::Write;

#[cfg(feature = "view")]
use html5ever::serialize::{HtmlSerializer, SerializeOpts, Serializer};
#[cfg(feature = "view")]
use html5ever::{Attribute, LocalName, QualName, namespace_url, ns};
use serde_json::Value;

use crate::RichTextNodeType;
use crate::model::Node;

/// Closed conversion from one semantic node attr to one DOM attribute.
#[derive(Clone, Debug)]
pub struct DomAttrBinding {
    destination: String,
    source: String,
    conversion: DomAttrConversion,
}

#[derive(Clone, Debug)]
enum DomAttrConversion {
    String,
    Bool,
    Integer,
    Token(Vec<String>),
    Url(UrlPolicy),
}

impl DomAttrBinding {
    pub fn string(destination: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(destination, source, DomAttrConversion::String)
    }

    pub fn boolean(destination: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(destination, source, DomAttrConversion::Bool)
    }

    pub fn integer(destination: impl Into<String>, source: impl Into<String>) -> Self {
        Self::new(destination, source, DomAttrConversion::Integer)
    }

    pub fn token(
        destination: impl Into<String>,
        source: impl Into<String>,
        allowed: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self::new(
            destination,
            source,
            DomAttrConversion::Token(allowed.into_iter().map(Into::into).collect()),
        )
    }

    pub fn url(
        destination: impl Into<String>,
        source: impl Into<String>,
        policy: UrlPolicy,
    ) -> Self {
        Self::new(destination, source, DomAttrConversion::Url(policy))
    }

    fn new(
        destination: impl Into<String>,
        source: impl Into<String>,
        conversion: DomAttrConversion,
    ) -> Self {
        Self {
            destination: destination.into(),
            source: source.into(),
            conversion,
        }
    }
}

/// Explicit policy required for URL-valued projections.
#[derive(Clone, Debug)]
pub struct UrlPolicy {
    allowed_schemes: BTreeSet<String>,
    allow_relative: bool,
}

impl UrlPolicy {
    pub fn new(allowed_schemes: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            allowed_schemes: allowed_schemes
                .into_iter()
                .map(|scheme| scheme.into().to_ascii_lowercase())
                .collect(),
            allow_relative: false,
        }
    }

    pub fn allow_relative(mut self, allow: bool) -> Self {
        self.allow_relative = allow;
        self
    }
}

/// One structural element. Dynamic values can only come from declared attrs.
#[derive(Clone, Debug)]
pub struct DomOutputSpec {
    tag: String,
    static_attrs: BTreeMap<String, String>,
    bindings: Vec<DomAttrBinding>,
    children: Vec<DomOutputChild>,
}

#[derive(Clone, Debug)]
enum DomOutputChild {
    Element(DomOutputSpec),
    ContentHole,
    TextBinding(String),
    StaticText(String),
}

impl DomOutputSpec {
    pub fn element(tag: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            static_attrs: BTreeMap::new(),
            bindings: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn class(self, class: impl Into<String>) -> Self {
        self.attr("class", class)
    }

    pub fn attr(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.static_attrs.insert(name.into(), value.into());
        self
    }

    pub fn bind_attr(mut self, binding: DomAttrBinding) -> Self {
        self.bindings.push(binding);
        self
    }

    pub fn child(mut self, child: DomOutputSpec) -> Self {
        self.children.push(DomOutputChild::Element(child));
        self
    }

    pub fn content_hole(mut self) -> Self {
        self.children.push(DomOutputChild::ContentHole);
        self
    }

    pub fn bind_text(mut self, source: impl Into<String>) -> Self {
        self.children
            .push(DomOutputChild::TextBinding(source.into()));
        self
    }

    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.children.push(DomOutputChild::StaticText(text.into()));
        self
    }
}

/// Typed semantic DOM fallback/native view.
#[derive(Clone, Debug)]
pub struct NodeDomSpec {
    semantic_type_id: TypeId,
    semantic_rust_type: &'static str,
    node_type: &'static str,
    output: DomOutputSpec,
}

impl NodeDomSpec {
    pub fn atom<N: RichTextNodeType>(tag: impl Into<String>) -> Self {
        Self::nested::<N>(DomOutputSpec::element(tag))
    }

    pub fn content<N: RichTextNodeType>(tag: impl Into<String>) -> Self {
        Self::nested::<N>(DomOutputSpec::element(tag).content_hole())
    }

    pub fn nested<N: RichTextNodeType>(output: DomOutputSpec) -> Self {
        Self {
            semantic_type_id: TypeId::of::<N>(),
            semantic_rust_type: std::any::type_name::<N>(),
            node_type: N::NAME,
            output,
        }
    }

    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.output = self.output.class(class);
        self
    }

    pub fn attr(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.output = self.output.attr(name, value);
        self
    }

    pub fn bind_attr(mut self, binding: DomAttrBinding) -> Self {
        self.output = self.output.bind_attr(binding);
        self
    }

    pub fn bind_text(mut self, source: impl Into<String>) -> Self {
        self.output = self.output.bind_text(source);
        self
    }

    pub fn semantic_type_id(&self) -> TypeId {
        self.semantic_type_id
    }

    pub fn semantic_rust_type(&self) -> &'static str {
        self.semantic_rust_type
    }

    pub fn node_type(&self) -> &'static str {
        self.node_type
    }

    pub fn root_tag(&self) -> &str {
        &self.output.tag
    }

    /// Build only the semantic outer host, without declared descendants.
    /// Typed component views mount into this empty structure so contextual
    /// parents see the correct native tag before the component shell exists.
    #[cfg(feature = "view")]
    pub(crate) fn root_element_spec(
        &self,
        node: &Node,
    ) -> Result<super::dom_output::DomElementSpec, NodeDomSpecError> {
        let mut values = self.output.static_attrs.clone();
        for binding in &self.output.bindings {
            if let Some(value) = project_attr(binding, node.attrs().get(&binding.source))? {
                values.insert(binding.destination.clone(), value);
            }
        }
        let mut root = super::dom_output::DomElementSpec::element(self.output.tag.clone())?;
        for (name, value) in values {
            root = root.attr(name, value)?;
        }
        Ok(root)
    }

    #[cfg(feature = "view")]
    pub(crate) fn root_binding_destinations(&self) -> impl Iterator<Item = &str> {
        self.output
            .bindings
            .iter()
            .map(|binding| binding.destination.as_str())
    }

    #[cfg(feature = "view")]
    pub(crate) fn content_hole_path(&self) -> Option<Vec<u16>> {
        fn find(output: &DomOutputSpec, path: &mut Vec<u16>) -> Option<Vec<u16>> {
            let mut element_index = 0u16;
            for child in &output.children {
                match child {
                    DomOutputChild::ContentHole => return Some(path.clone()),
                    DomOutputChild::Element(element) => {
                        path.push(element_index);
                        if let Some(found) = find(element, path) {
                            return Some(found);
                        }
                        path.pop();
                        element_index = element_index.saturating_add(1);
                    }
                    DomOutputChild::TextBinding(_) | DomOutputChild::StaticText(_) => {}
                }
            }
            None
        }
        find(&self.output, &mut Vec::new())
    }

    pub(crate) fn validate(&self, attr_keys: &[&str], atom: bool) -> Result<(), NodeDomSpecError> {
        let mut holes = 0;
        validate_output(&self.output, attr_keys, &mut holes)?;
        let expected = usize::from(!atom);
        if holes != expected {
            return Err(NodeDomSpecError::ContentHoleCount {
                node_type: self.node_type.to_string(),
                expected,
                found: holes,
            });
        }
        Ok(())
    }

    #[cfg(feature = "view")]
    pub(crate) fn render_into(
        &self,
        node: &Node,
        position: usize,
        content_html: &str,
        output: &mut String,
    ) -> Result<(), NodeDomSpecError> {
        let mut root = self.output.clone();
        root.static_attrs
            .insert("data-pos".to_string(), position.to_string());
        let mut bytes = Vec::new();
        let mut serializer = HtmlSerializer::new(&mut bytes, SerializeOpts::default());
        render_output(&root, node, content_html, &mut serializer)?;
        super::dom_output::append_serialized_bytes(output, bytes).map_err(Into::into)
    }

    /// Materialize the validated semantic structure without editor-only
    /// position stamps. Content-hole children stay typed structural nodes;
    /// component/view DOM and raw HTML are never accepted here.
    pub(crate) fn semantic_node(
        &self,
        node: &Node,
        content: &[super::dom_output::DomNodeSpec],
    ) -> Result<super::dom_output::DomNodeSpec, NodeDomSpecError> {
        semantic_output_node(&self.output, node, content)
            .map(super::dom_output::DomNodeSpec::Element)
    }

    /// Build the native in-editor structure as typed nodes. The only
    /// difference from semantic export is the framework-owned `data-pos`
    /// marker; content-hole children remain structural and never cross a raw
    /// HTML boundary.
    pub(crate) fn editor_node(
        &self,
        node: &Node,
        position: usize,
        content: &[super::dom_output::DomNodeSpec],
    ) -> Result<super::dom_output::DomNodeSpec, NodeDomSpecError> {
        let mut output = self.output.clone();
        output
            .static_attrs
            .insert("data-pos".to_string(), position.to_string());
        semantic_output_node(&output, node, content).map(super::dom_output::DomNodeSpec::Element)
    }

    /// Render the declared contents without repeating the outer semantic host.
    /// Component-failure recovery uses this to turn the already-correct typed
    /// host into the native fallback instead of nesting (for example) a second
    /// `<table>` inside the first one.
    #[cfg(feature = "view")]
    #[cfg(feature = "view")]
    pub(crate) fn render_inner_into(
        &self,
        node: &Node,
        content_html: &str,
        output: &mut String,
    ) -> Result<(), NodeDomSpecError> {
        let mut bytes = Vec::new();
        let mut serializer = HtmlSerializer::new(&mut bytes, SerializeOpts::default());
        for child in &self.output.children {
            render_output_child(child, node, content_html, &mut serializer)?;
        }
        super::dom_output::append_serialized_bytes(output, bytes).map_err(Into::into)
    }
}

fn validate_output(
    output: &DomOutputSpec,
    attr_keys: &[&str],
    holes: &mut usize,
) -> Result<(), NodeDomSpecError> {
    super::dom_output::DomElementSpec::element(output.tag.clone())?;
    let mut destinations = BTreeSet::new();
    for name in output.static_attrs.keys() {
        super::dom_output::DomElementSpec::element("div")?.attr(name, "")?;
        if is_url_attribute(name) {
            return Err(NodeDomSpecError::UrlPolicyRequired {
                destination: name.clone(),
            });
        }
        destinations.insert(name.clone());
    }
    for binding in &output.bindings {
        super::dom_output::DomElementSpec::element("div")?.attr(&binding.destination, "")?;
        if is_url_attribute(&binding.destination)
            && !matches!(&binding.conversion, DomAttrConversion::Url(_))
        {
            return Err(NodeDomSpecError::UrlPolicyRequired {
                destination: binding.destination.clone(),
            });
        }
        if let DomAttrConversion::Url(policy) = &binding.conversion {
            for scheme in &policy.allowed_schemes {
                let valid = !scheme.is_empty()
                    && scheme.chars().enumerate().all(|(index, ch)| {
                        ch.is_ascii_alphabetic()
                            || (index > 0 && (ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.')))
                    });
                if !valid || matches!(scheme.as_str(), "javascript" | "vbscript" | "data") {
                    return Err(NodeDomSpecError::UnsafeUrlScheme(scheme.clone()));
                }
            }
        }
        if !attr_keys.contains(&binding.source.as_str()) {
            return Err(NodeDomSpecError::UnknownSourceAttr {
                source: binding.source.clone(),
            });
        }
        if !destinations.insert(binding.destination.clone()) {
            return Err(NodeDomSpecError::DuplicateDestination {
                destination: binding.destination.clone(),
            });
        }
    }
    for child in &output.children {
        match child {
            DomOutputChild::Element(child) => validate_output(child, attr_keys, holes)?,
            DomOutputChild::ContentHole => *holes += 1,
            DomOutputChild::TextBinding(source) => {
                if !attr_keys.contains(&source.as_str()) {
                    return Err(NodeDomSpecError::UnknownSourceAttr {
                        source: source.clone(),
                    });
                }
            }
            DomOutputChild::StaticText(_) => {}
        }
    }
    Ok(())
}

fn semantic_output_node(
    output: &DomOutputSpec,
    node: &Node,
    content: &[super::dom_output::DomNodeSpec],
) -> Result<super::dom_output::DomElementSpec, NodeDomSpecError> {
    let mut element = super::dom_output::DomElementSpec::element(output.tag.clone())?;
    let mut values = output.static_attrs.clone();
    for binding in &output.bindings {
        if let Some(value) = project_attr(binding, node.attrs().get(&binding.source))? {
            values.insert(binding.destination.clone(), value);
        }
    }
    for (name, value) in values {
        element = element.attr(name, value)?;
    }
    for child in &output.children {
        element = match child {
            DomOutputChild::Element(child) => {
                element.node(semantic_output_node(child, node, content)?)?
            }
            DomOutputChild::ContentHole => element.extend_nodes(content.iter().cloned())?,
            DomOutputChild::TextBinding(source) => {
                let text = node
                    .attrs()
                    .get(source)
                    .and_then(Value::as_str)
                    .ok_or_else(|| NodeDomSpecError::InvalidValue {
                        source: source.clone(),
                        expected: "string",
                    })?;
                element.text(text)?
            }
            DomOutputChild::StaticText(text) => element.text(text)?,
        };
    }
    Ok(element)
}

#[cfg(feature = "view")]
fn render_output<W: Write>(
    output: &DomOutputSpec,
    node: &Node,
    content_html: &str,
    serializer: &mut HtmlSerializer<W>,
) -> Result<(), NodeDomSpecError> {
    let name = QualName::new(None, ns!(html), LocalName::from(output.tag.as_str()));
    let mut values = output.static_attrs.clone();
    for binding in &output.bindings {
        if let Some(value) = project_attr(binding, node.attrs().get(&binding.source))? {
            values.insert(binding.destination.clone(), value);
        }
    }
    let attrs = values
        .iter()
        .map(|(name, value)| Attribute {
            name: QualName::new(None, ns!(), LocalName::from(name.as_str())),
            value: value.as_str().into(),
        })
        .collect::<Vec<_>>();
    serializer
        .start_elem(
            name.clone(),
            attrs.iter().map(|attr| (&attr.name, attr.value.as_ref())),
        )
        .map_err(|error| NodeDomSpecError::Serialize(error.to_string()))?;
    for child in &output.children {
        render_output_child(child, node, content_html, serializer)?;
    }
    serializer
        .end_elem(name)
        .map_err(|error| NodeDomSpecError::Serialize(error.to_string()))
}

#[cfg(feature = "view")]
fn render_output_child<W: Write>(
    child: &DomOutputChild,
    node: &Node,
    content_html: &str,
    serializer: &mut HtmlSerializer<W>,
) -> Result<(), NodeDomSpecError> {
    match child {
        DomOutputChild::Element(child) => render_output(child, node, content_html, serializer),
        DomOutputChild::ContentHole => serializer
            .writer
            .write_all(content_html.as_bytes())
            .map_err(|error| NodeDomSpecError::Serialize(error.to_string())),
        DomOutputChild::TextBinding(source) => {
            let text = node
                .attrs()
                .get(source)
                .and_then(Value::as_str)
                .ok_or_else(|| NodeDomSpecError::InvalidValue {
                    source: source.clone(),
                    expected: "string",
                })?;
            serializer
                .write_text(text)
                .map_err(|error| NodeDomSpecError::Serialize(error.to_string()))
        }
        DomOutputChild::StaticText(text) => serializer
            .write_text(text)
            .map_err(|error| NodeDomSpecError::Serialize(error.to_string())),
    }
}

fn project_attr(
    binding: &DomAttrBinding,
    value: Option<&Value>,
) -> Result<Option<String>, NodeDomSpecError> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let invalid = |expected| NodeDomSpecError::InvalidValue {
        source: binding.source.clone(),
        expected,
    };
    let projected = match &binding.conversion {
        DomAttrConversion::String => value.as_str().ok_or_else(|| invalid("string"))?.to_string(),
        DomAttrConversion::Bool => value
            .as_bool()
            .ok_or_else(|| invalid("boolean"))?
            .to_string(),
        DomAttrConversion::Integer => value
            .as_i64()
            .map(|value| value.to_string())
            .or_else(|| value.as_u64().map(|value| value.to_string()))
            .ok_or_else(|| invalid("integer"))?,
        DomAttrConversion::Token(allowed) => {
            let token = value.as_str().ok_or_else(|| invalid("token string"))?;
            if !allowed.iter().any(|allowed| allowed == token) {
                return Err(NodeDomSpecError::InvalidToken {
                    source: binding.source.clone(),
                    token: token.to_string(),
                });
            }
            token.to_string()
        }
        DomAttrConversion::Url(policy) => {
            let url = value.as_str().ok_or_else(|| invalid("URL string"))?;
            validate_url(url, policy)?;
            url.to_string()
        }
    };
    Ok(Some(projected))
}

fn validate_url(url: &str, policy: &UrlPolicy) -> Result<(), NodeDomSpecError> {
    if url.chars().any(char::is_control) {
        return Err(NodeDomSpecError::UnsafeUrl(url.to_string()));
    }
    let before_path = url.split(['/', '?', '#']).next().unwrap_or_default();
    if let Some((scheme, _)) = before_path.split_once(':') {
        let valid_scheme = !scheme.is_empty()
            && scheme.chars().enumerate().all(|(index, ch)| {
                ch.is_ascii_alphabetic()
                    || (index > 0 && (ch.is_ascii_digit() || matches!(ch, '+' | '-' | '.')))
            });
        if !valid_scheme
            || !policy
                .allowed_schemes
                .contains(&scheme.to_ascii_lowercase())
        {
            return Err(NodeDomSpecError::UnsafeUrl(url.to_string()));
        }
    } else if !policy.allow_relative {
        return Err(NodeDomSpecError::UnsafeUrl(url.to_string()));
    }
    Ok(())
}

fn is_url_attribute(name: &str) -> bool {
    matches!(
        name,
        "href"
            | "src"
            | "action"
            | "formaction"
            | "poster"
            | "cite"
            | "background"
            | "xlink:href"
            | "srcset"
            | "ping"
            | "manifest"
            | "codebase"
            | "archive"
    )
}

#[derive(Debug)]
pub enum NodeDomSpecError {
    Output(super::dom_output::DomOutputError),
    UnknownSourceAttr {
        source: String,
    },
    DuplicateDestination {
        destination: String,
    },
    UrlPolicyRequired {
        destination: String,
    },
    UnsafeUrlScheme(String),
    ContentHoleCount {
        node_type: String,
        expected: usize,
        found: usize,
    },
    InvalidValue {
        source: String,
        expected: &'static str,
    },
    InvalidToken {
        source: String,
        token: String,
    },
    UnsafeUrl(String),
    Serialize(String),
}

impl From<super::dom_output::DomOutputError> for NodeDomSpecError {
    fn from(error: super::dom_output::DomOutputError) -> Self {
        Self::Output(error)
    }
}

impl fmt::Display for NodeDomSpecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output(error) => write!(formatter, "{error}"),
            Self::UnknownSourceAttr { source } => {
                write!(formatter, "unknown source attr `{source}`")
            }
            Self::DuplicateDestination { destination } => {
                write!(formatter, "duplicate DOM attr destination `{destination}`")
            }
            Self::UrlPolicyRequired { destination } => {
                write!(
                    formatter,
                    "URL-valued DOM attr `{destination}` requires DomAttrBinding::url and an explicit UrlPolicy"
                )
            }
            Self::UnsafeUrlScheme(scheme) => {
                write!(
                    formatter,
                    "URL policy cannot allow unsafe scheme `{scheme}`"
                )
            }
            Self::ContentHoleCount {
                node_type,
                expected,
                found,
            } => write!(
                formatter,
                "DOM view for `{node_type}` requires {expected} content hole(s), found {found}"
            ),
            Self::InvalidValue { source, expected } => {
                write!(formatter, "attr `{source}` is not a valid {expected}")
            }
            Self::InvalidToken { source, token } => {
                write!(formatter, "attr `{source}` has disallowed token `{token}`")
            }
            Self::UnsafeUrl(url) => {
                write!(formatter, "URL `{url}` is outside the declared URL policy")
            }
            Self::Serialize(message) => {
                write!(formatter, "DOM view serialization failed: {message}")
            }
        }
    }
}

impl std::error::Error for NodeDomSpecError {}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::model::{Attrs, Fragment, NodeSpec};
    use crate::{RichTextNodeAttrs, RichTextNodeType};

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, RichTextNodeAttrs)]
    struct LinkAttrs {
        href: String,
        label: String,
    }

    struct LinkNode;

    impl RichTextNodeType for LinkNode {
        const NAME: &'static str = "safe_link";
        const VERSION: u32 = 1;
        type Attrs = LinkAttrs;

        fn spec() -> NodeSpec {
            NodeSpec::new(Self::NAME)
                .atom()
                .required_attr("href")
                .required_attr("label")
        }
    }

    #[test]
    fn active_tags_event_attrs_and_untyped_url_bindings_fail_closed() {
        let script = NodeDomSpec::atom::<LinkNode>("script");
        assert!(matches!(
            script.validate(LinkAttrs::KEYS, true),
            Err(NodeDomSpecError::Output(
                super::super::dom_output::DomOutputError::InvalidTag(_)
            ))
        ));

        let event = NodeDomSpec::atom::<LinkNode>("a").attr("onclick", "steal()");
        assert!(matches!(
            event.validate(LinkAttrs::KEYS, true),
            Err(NodeDomSpecError::Output(
                super::super::dom_output::DomOutputError::EventAttribute(_)
            ))
        ));

        let string_url =
            NodeDomSpec::atom::<LinkNode>("a").bind_attr(DomAttrBinding::string("href", "href"));
        assert!(matches!(
            string_url.validate(LinkAttrs::KEYS, true),
            Err(NodeDomSpecError::UrlPolicyRequired { .. })
        ));

        let unsafe_policy = NodeDomSpec::atom::<LinkNode>("a").bind_attr(DomAttrBinding::url(
            "href",
            "href",
            UrlPolicy::new(["javascript"]),
        ));
        assert!(matches!(
            unsafe_policy.validate(LinkAttrs::KEYS, true),
            Err(NodeDomSpecError::UnsafeUrlScheme(_))
        ));
    }

    #[test]
    fn url_policy_rejects_javascript_and_semantic_text_is_escaped() {
        let spec = NodeDomSpec::atom::<LinkNode>("a")
            .bind_attr(DomAttrBinding::url(
                "href",
                "href",
                UrlPolicy::new(["https"]).allow_relative(true),
            ))
            .bind_text("label");
        spec.validate(LinkAttrs::KEYS, true).unwrap();

        let attrs = Attrs::from([
            ("href".to_string(), json!("JaVaScRiPt:alert(1)")),
            ("label".to_string(), json!("<unsafe>")),
        ]);
        let unsafe_node = Node::unchecked(
            LinkNode::NAME.to_string(),
            attrs,
            Vec::new(),
            Fragment::empty(),
            None,
            true,
        );
        assert!(matches!(
            spec.semantic_node(&unsafe_node, &[]),
            Err(NodeDomSpecError::UnsafeUrl(_))
        ));

        let attrs = Attrs::from([
            ("href".to_string(), json!("/safe")),
            ("label".to_string(), json!("<safe>")),
        ]);
        let safe_node = Node::unchecked(
            LinkNode::NAME.to_string(),
            attrs,
            Vec::new(),
            Fragment::empty(),
            None,
            true,
        );
        let mut html = String::new();
        spec.semantic_node(&safe_node, &[])
            .unwrap()
            .compile_into(&mut html)
            .unwrap();
        assert_eq!(html, r#"<a href="/safe">&lt;safe&gt;</a>"#);
    }
}
