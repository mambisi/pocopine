//! Validated structural DOM output plans.
//!
//! Extension and interactive rendering code builds this data structure. A
//! single compiler owns HTML syntax/escaping, so feature code never assembles
//! tags with interleaved `push_str` calls.

use std::collections::BTreeMap;
use std::fmt;

use html5ever::serialize::{HtmlSerializer, SerializeOpts, Serializer};
use html5ever::{Attribute, LocalName, QualName, namespace_url, ns};

/// A validated element plus its static/deterministically projected attrs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DomElementSpec {
    tag: String,
    attrs: BTreeMap<String, String>,
    children: Vec<DomNodeSpec>,
}

/// One safe semantic output node. Text stays unescaped until the shared
/// html5ever compiler writes it, so mixed inline text/element content never
/// requires raw HTML concatenation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomNodeSpec {
    /// A validated structural element.
    Element(DomElementSpec),
    /// Text escaped exactly once by the shared serializer.
    Text(String),
}

impl DomNodeSpec {
    /// Construct an escaped text node.
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(value.into())
    }

    /// Compile this structural node through the shared html5ever sink.
    pub fn compile_into(&self, output: &mut String) -> Result<(), DomOutputError> {
        compile_nodes(std::slice::from_ref(self), output)
    }

    /// Materialize this structural node directly through browser DOM APIs.
    ///
    /// Reconciliation uses this instead of reparsing a serialized fragment in
    /// the element being replaced. Context-sensitive HTML such as `<tr>` and
    /// `<td>` therefore keeps its declared hierarchy regardless of the old
    /// node's parsing context.
    #[cfg(feature = "view")]
    pub fn materialize(
        &self,
        document: &web_sys::Document,
    ) -> Result<web_sys::Node, DomOutputError> {
        match self {
            Self::Element(element) => element.materialize(document).map(Into::into),
            Self::Text(text) => Ok(document.create_text_node(text).into()),
        }
    }

    fn serialize_with<W: std::io::Write>(
        &self,
        serializer: &mut HtmlSerializer<W>,
    ) -> std::io::Result<()> {
        match self {
            Self::Element(element) => element.serialize_with(serializer),
            Self::Text(text) => serializer.write_text(text),
        }
    }
}

impl From<DomElementSpec> for DomNodeSpec {
    fn from(value: DomElementSpec) -> Self {
        Self::Element(value)
    }
}

/// Wrapperless sequence of safe semantic output nodes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DomFragmentSpec {
    children: Vec<DomNodeSpec>,
}

impl DomFragmentSpec {
    /// Construct an empty semantic fragment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one structural node.
    pub fn node(mut self, node: impl Into<DomNodeSpec>) -> Self {
        self.children.push(node.into());
        self
    }

    /// Append structural nodes in order.
    pub fn extend(mut self, nodes: impl IntoIterator<Item = DomNodeSpec>) -> Self {
        self.children.extend(nodes);
        self
    }

    /// Borrow the structural children.
    pub fn children(&self) -> &[DomNodeSpec] {
        &self.children
    }

    /// Compile the whole fragment through one shared html5ever sink.
    pub fn compile_into(&self, output: &mut String) -> Result<(), DomOutputError> {
        compile_nodes(&self.children, output)
    }
}

impl DomElementSpec {
    /// Start a structural element plan.
    pub fn element(tag: impl Into<String>) -> Result<Self, DomOutputError> {
        let tag = tag.into();
        validate_tag(&tag)?;
        Ok(Self {
            tag,
            attrs: BTreeMap::new(),
            children: Vec::new(),
        })
    }

    /// Add a validated attribute. Duplicate names replace the earlier value
    /// deterministically.
    pub fn attr(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, DomOutputError> {
        let name = name.into();
        validate_attr(&name)?;
        self.attrs.insert(name, value.into());
        Ok(self)
    }

    /// Add one structural child element.
    pub fn child(mut self, child: DomElementSpec) -> Result<Self, DomOutputError> {
        self.children.push(DomNodeSpec::Element(child));
        Ok(self)
    }

    /// Append escaped text content. Text and elements may be freely mixed;
    /// ordering remains structural until serialization.
    pub fn text(mut self, text: impl Into<String>) -> Result<Self, DomOutputError> {
        self.children.push(DomNodeSpec::Text(text.into()));
        Ok(self)
    }

    /// Append one mixed structural child.
    pub fn node(mut self, child: impl Into<DomNodeSpec>) -> Result<Self, DomOutputError> {
        self.children.push(child.into());
        Ok(self)
    }

    /// Append mixed structural children.
    pub fn extend_nodes(
        mut self,
        children: impl IntoIterator<Item = DomNodeSpec>,
    ) -> Result<Self, DomOutputError> {
        self.children.extend(children);
        Ok(self)
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    pub fn attrs(&self) -> &BTreeMap<String, String> {
        &self.attrs
    }

    /// Materialize the same structural plan directly through browser DOM APIs.
    /// Interactive rendering uses this backend, avoiding an HTML parse just to
    /// create a typed node-view host.
    #[cfg(feature = "view")]
    pub fn materialize(
        &self,
        document: &web_sys::Document,
    ) -> Result<web_sys::Element, DomOutputError> {
        let element = document
            .create_element(&self.tag)
            .map_err(|error| DomOutputError::Dom(format!("create <{}>: {error:?}", self.tag)))?;
        for (name, value) in &self.attrs {
            element.set_attribute(name, value).map_err(|error| {
                DomOutputError::Dom(format!("set `{name}` on <{}>: {error:?}", self.tag))
            })?;
        }
        for child in &self.children {
            let child = child.materialize(document)?;
            element.append_child(&child).map_err(|error| {
                DomOutputError::Dom(format!("append child to <{}>: {error:?}", self.tag))
            })?;
        }
        Ok(element)
    }

    /// Compile this already-validated plan to HTML.
    pub fn compile_into(&self, output: &mut String) -> Result<(), DomOutputError> {
        DomNodeSpec::Element(self.clone()).compile_into(output)
    }

    fn serialize_with<W: std::io::Write>(
        &self,
        serializer: &mut HtmlSerializer<W>,
    ) -> std::io::Result<()> {
        let name = QualName::new(None, ns!(html), LocalName::from(self.tag.as_str()));
        let attrs = self
            .attrs
            .iter()
            .map(|(name, value)| Attribute {
                name: QualName::new(None, ns!(), LocalName::from(name.as_str())),
                value: value.as_str().into(),
            })
            .collect::<Vec<_>>();
        serializer.start_elem(
            name.clone(),
            attrs.iter().map(|attr| (&attr.name, attr.value.as_ref())),
        )?;
        for child in &self.children {
            child.serialize_with(serializer)?;
        }
        serializer.end_elem(name)
    }
}

fn compile_nodes(nodes: &[DomNodeSpec], output: &mut String) -> Result<(), DomOutputError> {
    let mut bytes = Vec::new();
    let mut serializer = HtmlSerializer::new(&mut bytes, SerializeOpts::default());
    for node in nodes {
        node.serialize_with(&mut serializer)
            .map_err(|error| DomOutputError::Serialize(error.to_string()))?;
    }
    append_serialized_bytes(output, bytes)
}

/// Finish one html5ever compilation. Keeping UTF-8 conversion and string
/// appending here makes this the sole compiler sink in the render pipeline.
pub(crate) fn append_serialized_bytes(
    output: &mut String,
    bytes: Vec<u8>,
) -> Result<(), DomOutputError> {
    let html =
        String::from_utf8(bytes).map_err(|error| DomOutputError::Serialize(error.to_string()))?;
    output.push_str(&html);
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DomOutputError {
    InvalidTag(String),
    InvalidAttribute(String),
    EventAttribute(String),
    Serialize(String),
    Dom(String),
}

impl fmt::Display for DomOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTag(tag) => write!(formatter, "invalid DOM tag `{tag}`"),
            Self::InvalidAttribute(attr) => write!(formatter, "invalid DOM attribute `{attr}`"),
            Self::EventAttribute(attr) => {
                write!(
                    formatter,
                    "event-handler DOM attribute `{attr}` is forbidden"
                )
            }
            Self::Serialize(message) => {
                write!(formatter, "DOM output serialization failed: {message}")
            }
            Self::Dom(message) => write!(formatter, "DOM output materialization failed: {message}"),
        }
    }
}

impl std::error::Error for DomOutputError {}

fn validate_tag(tag: &str) -> Result<(), DomOutputError> {
    let mut chars = tag.chars();
    let Some(first) = chars.next() else {
        return Err(DomOutputError::InvalidTag(tag.to_string()));
    };
    if !first.is_ascii_lowercase()
        || !chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(DomOutputError::InvalidTag(tag.to_string()));
    }
    if matches!(
        tag,
        "script"
            | "style"
            | "iframe"
            | "object"
            | "embed"
            | "link"
            | "meta"
            | "base"
            | "form"
            | "input"
            | "button"
            | "select"
            | "textarea"
            | "option"
            | "svg"
            | "math"
    ) {
        return Err(DomOutputError::InvalidTag(tag.to_string()));
    }
    Ok(())
}

fn validate_attr(attr: &str) -> Result<(), DomOutputError> {
    if attr
        .get(..2)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("on"))
    {
        return Err(DomOutputError::EventAttribute(attr.to_string()));
    }
    if matches!(attr, "style" | "srcdoc") {
        return Err(DomOutputError::InvalidAttribute(attr.to_string()));
    }
    let mut chars = attr.chars();
    let Some(first) = chars.next() else {
        return Err(DomOutputError::InvalidAttribute(attr.to_string()));
    };
    if !first.is_ascii_lowercase()
        || !chars.all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | ':')
        })
    {
        return Err(DomOutputError::InvalidAttribute(attr.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_plan_compiles_and_escapes_once() {
        let plan = DomElementSpec::element("div")
            .unwrap()
            .attr("data-kind", "a\"b")
            .unwrap()
            .child(
                DomElementSpec::element("span")
                    .unwrap()
                    .text("<safe>")
                    .unwrap(),
            )
            .unwrap();
        let mut html = String::new();
        plan.compile_into(&mut html).unwrap();
        assert_eq!(
            html,
            "<div data-kind=\"a&quot;b\"><span>&lt;safe&gt;</span></div>"
        );
    }

    #[test]
    fn structural_plan_preserves_mixed_text_and_element_order() {
        let plan = DomElementSpec::element("p")
            .unwrap()
            .text("before <")
            .unwrap()
            .child(
                DomElementSpec::element("strong")
                    .unwrap()
                    .text("middle &")
                    .unwrap(),
            )
            .unwrap()
            .text(" > after")
            .unwrap();
        let mut html = String::new();
        plan.compile_into(&mut html).unwrap();
        assert_eq!(
            html,
            "<p>before &lt;<strong>middle &amp;</strong> &gt; after</p>"
        );
    }

    #[test]
    fn invalid_and_event_attributes_fail_closed() {
        assert!(matches!(
            DomElementSpec::element("DIV"),
            Err(DomOutputError::InvalidTag(_))
        ));
        assert!(matches!(
            DomElementSpec::element("div").unwrap().attr("onclick", "x"),
            Err(DomOutputError::EventAttribute(_))
        ));
    }
}
