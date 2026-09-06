use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use crate::{CardinalRule, DateTimeArg, Locale, PluralArg};

const MAX_MESSAGE_BYTES: usize = 65_536;
const MAX_NODES: usize = 4_096;
const MAX_CHOICE_DEPTH: usize = 2;
const MAX_ELEMENT_DEPTH: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentKind {
    Text,
    Number,
    DateTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NumberStyle {
    Decimal,
    Percent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StyleLength {
    Short,
    Medium,
    Long,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DateTimeStyle {
    Date(StyleLength),
    Time(StyleLength),
}

/// Typed input to the shared message interpreter. Date/time inputs include an
/// explicit recipient timezone as well as their Unix timestamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value<'a> {
    Text(&'a str),
    Number(PluralArg),
    DateTime(&'a DateTimeArg),
}

impl Value<'_> {
    fn kind(self) -> ArgumentKind {
        match self {
            Self::Text(_) => ArgumentKind::Text,
            Self::Number(_) => ArgumentKind::Number,
            Self::DateTime(_) => ArgumentKind::DateTime,
        }
    }
}

/// Selected message content. Number/date rendering is deliberately deferred
/// to the host/browser formatter; branch selection is already final and shared.
/// Element markers refer to existing template children, never translated HTML.
#[derive(Clone, Debug, PartialEq)]
pub enum MessagePart<'a> {
    Text(Cow<'a, str>),
    Number {
        value: PluralArg,
        style: NumberStyle,
    },
    DateTime {
        value: &'a DateTimeArg,
        style: DateTimeStyle,
    },
    OpenElement(u16),
    CloseElement(u16),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageError {
    /// UTF-8 byte offset into the message source.
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}
impl std::error::Error for MessageError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FormatError {
    MissingArgument(String),
    ExtraArgument(String),
    DuplicateArgument(String),
    ArgumentType {
        name: String,
        expected: ArgumentKind,
    },
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingArgument(name) => write!(f, "missing message argument {name}"),
            Self::ExtraArgument(name) => write!(f, "unexpected message argument {name}"),
            Self::DuplicateArgument(name) => write!(f, "duplicate message argument {name}"),
            Self::ArgumentType { name, expected } => {
                write!(f, "message argument {name} requires {expected:?}")
            }
        }
    }
}
impl std::error::Error for FormatError {}

#[derive(Clone, Debug)]
enum Node {
    Text(String),
    Argument(String),
    Number(String, NumberStyle),
    DateTime(String, DateTimeStyle),
    Choice {
        name: String,
        plural: bool,
        variants: Vec<(Selector, Vec<Node>)>,
    },
    Element(u16, Vec<Node>),
}

#[derive(Clone, Debug)]
enum Selector {
    Named(String),
    Exact(PluralArg),
}

/// A validated ICU MessageFormat 1 message in RFC-120's closed subset.
/// Parse once when compiling/loading a catalog, then reuse across requests.
#[derive(Clone, Debug)]
pub struct Message {
    nodes: Vec<Node>,
    arguments: BTreeMap<String, ArgumentKind>,
    elements: BTreeSet<u16>,
}

impl Message {
    pub fn parse(source: &str) -> Result<Self, MessageError> {
        if source.len() > MAX_MESSAGE_BYTES {
            return Err(MessageError {
                offset: 0,
                message: "message exceeds 65536 bytes".into(),
            });
        }
        let mut parser = Parser {
            source,
            offset: 0,
            nodes: 0,
            arguments: BTreeMap::new(),
        };
        let nodes = parser.sequence(None, None, 0, 0)?;
        let elements =
            element_contract(&nodes).map_err(|message| MessageError { offset: 0, message })?;
        let arguments = parser
            .arguments
            .into_iter()
            .map(|(name, kind)| (name, kind.unwrap_or(ArgumentKind::Text)))
            .collect();
        Ok(Self {
            nodes,
            arguments,
            elements,
        })
    }

    /// Inferred signature for code generation and cross-locale validation.
    pub fn arguments(&self) -> &BTreeMap<String, ArgumentKind> {
        &self.arguments
    }
    pub fn elements(&self) -> &BTreeSet<u16> {
        &self.elements
    }

    /// Select all plural/select branches before platform-specific formatting.
    /// Checks the entire argument contract, including inactive branches.
    pub fn parts<'a>(
        &'a self,
        locale: &Locale,
        args: &'a [(&str, Value<'a>)],
    ) -> Result<Vec<MessagePart<'a>>, FormatError> {
        let mut seen = BTreeSet::new();
        for (name, value) in args {
            if !seen.insert(*name) {
                return Err(FormatError::DuplicateArgument((*name).into()));
            }
            let Some(expected) = self.arguments.get(*name) else {
                return Err(FormatError::ExtraArgument((*name).into()));
            };
            if *expected != value.kind() {
                return Err(FormatError::ArgumentType {
                    name: (*name).into(),
                    expected: *expected,
                });
            }
        }
        for name in self.arguments.keys() {
            if !seen.contains(name.as_str()) {
                return Err(FormatError::MissingArgument(name.clone()));
            }
        }
        let mut parts = Vec::new();
        select(
            &self.nodes,
            CardinalRule::for_locale(locale),
            args,
            &mut parts,
        );
        Ok(parts)
    }
}

fn select<'a>(
    nodes: &'a [Node],
    rule: CardinalRule,
    args: &'a [(&str, Value<'a>)],
    out: &mut Vec<MessagePart<'a>>,
) {
    let get = |name: &str| {
        args.iter()
            .find(|(key, _)| *key == name)
            .expect("checked argument contract")
            .1
    };
    for node in nodes {
        match node {
            Node::Text(text) => out.push(MessagePart::Text(Cow::Borrowed(text))),
            Node::Argument(name) => out.push(match get(name) {
                Value::Text(text) => MessagePart::Text(Cow::Borrowed(text)),
                Value::Number(value) => MessagePart::Number {
                    value,
                    style: NumberStyle::Decimal,
                },
                Value::DateTime(value) => MessagePart::DateTime {
                    value,
                    style: DateTimeStyle::Date(StyleLength::Medium),
                },
            }),
            Node::Number(name, style) => {
                let Value::Number(value) = get(name) else {
                    unreachable!("checked argument contract")
                };
                out.push(MessagePart::Number {
                    value,
                    style: *style,
                });
            }
            Node::DateTime(name, style) => {
                let Value::DateTime(value) = get(name) else {
                    unreachable!("checked argument contract")
                };
                out.push(MessagePart::DateTime {
                    value,
                    style: *style,
                });
            }
            Node::Element(index, children) => {
                out.push(MessagePart::OpenElement(*index));
                select(children, rule, args, out);
                out.push(MessagePart::CloseElement(*index));
            }
            Node::Choice {
                name,
                plural,
                variants,
            } => {
                let value = get(name);
                let exact = if let Value::Number(n) = value {
                    variants.iter().find(
                        |(selector, _)| matches!(selector, Selector::Exact(x) if n.numeric_eq(*x)),
                    )
                } else {
                    None
                };
                let category = match value {
                    Value::Number(n) if *plural => rule.category(n).as_str(),
                    Value::Text(text) => text,
                    _ => unreachable!("checked argument contract"),
                };
                let named = |wanted: &str| {
                    variants
                        .iter()
                        .find(|(s, _)| matches!(s, Selector::Named(name) if name == wanted))
                };
                let (_, selected) = exact
                    .or_else(|| named(category))
                    .or_else(|| named("other"))
                    .expect("parser requires other");
                select(selected, rule, args, out);
            }
        }
    }
}

/// Alternatives may reorder the same elements, but no render path may consume
/// the same template child twice. Branches must preserve the element set.
fn element_contract(nodes: &[Node]) -> Result<BTreeSet<u16>, String> {
    let mut used = BTreeSet::new();
    for node in nodes {
        let nested = match node {
            Node::Element(index, children) => {
                let mut indices = element_contract(children)?;
                if !indices.insert(*index) {
                    return Err(format!("duplicate element placeholder <{index}>"));
                }
                indices
            }
            Node::Choice { variants, .. } => {
                let mut sets = variants.iter().map(|(_, nodes)| element_contract(nodes));
                let first = sets.next().transpose()?.unwrap_or_default();
                for set in sets {
                    if set? != first {
                        return Err(
                            "message branches must preserve the same element placeholders".into(),
                        );
                    }
                }
                first
            }
            _ => continue,
        };
        for index in nested {
            if !used.insert(index) {
                return Err(format!("duplicate element placeholder <{index}>"));
            }
        }
    }
    Ok(used)
}

struct Parser<'a> {
    source: &'a str,
    offset: usize,
    nodes: usize,
    arguments: BTreeMap<String, Option<ArgumentKind>>,
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> MessageError {
        MessageError {
            offset: self.offset,
            message: message.into(),
        }
    }
    fn rest(&self) -> &str {
        &self.source[self.offset..]
    }
    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }
    fn bump(&mut self) -> Option<char> {
        let next = self.peek()?;
        self.offset += next.len_utf8();
        Some(next)
    }
    fn whitespace(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.bump();
        }
    }
    fn expect(&mut self, c: char) -> Result<(), MessageError> {
        if self.peek() != Some(c) {
            return Err(self.error(format!("expected {c:?}")));
        }
        self.bump();
        Ok(())
    }
    fn identifier(&mut self) -> Result<String, MessageError> {
        let start = self.offset;
        if !self
            .peek()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        {
            return Err(self.error("expected an argument or selector identifier"));
        }
        self.bump();
        while self
            .peek()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            self.bump();
        }
        Ok(self.source[start..self.offset].into())
    }
    fn argument(&mut self, name: &str, kind: Option<ArgumentKind>) -> Result<(), MessageError> {
        let current = self.arguments.get(name).copied().flatten();
        if let (Some(a), Some(b)) = (current, kind)
            && a != b
        {
            return Err(self.error(format!("conflicting types for argument {name}")));
        }
        self.arguments.insert(name.into(), current.or(kind));
        Ok(())
    }
    fn push(&mut self, out: &mut Vec<Node>, node: Node) -> Result<(), MessageError> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(self.error("message exceeds 4096 nodes"));
        }
        out.push(node);
        Ok(())
    }
    fn flush(&mut self, out: &mut Vec<Node>, text: &mut String) -> Result<(), MessageError> {
        if !text.is_empty() {
            self.push(out, Node::Text(std::mem::take(text)))?;
        }
        Ok(())
    }

    fn sequence(
        &mut self,
        end_element: Option<u16>,
        plural: Option<&str>,
        choices: usize,
        elements: usize,
    ) -> Result<Vec<Node>, MessageError> {
        let mut out = Vec::new();
        let mut text = String::new();
        while let Some(c) = self.peek() {
            match c {
                '}' if end_element.is_some() => {
                    return Err(
                        self.error("element placeholder must close before its message branch")
                    );
                }
                '}' if choices > 0 => break,
                '}' => return Err(self.error("unmatched closing brace")),
                '{' => {
                    self.flush(&mut out, &mut text)?;
                    let node = self.placeholder(plural, choices, elements)?;
                    self.push(&mut out, node)?;
                }
                '#' if plural.is_some() => {
                    self.bump();
                    self.flush(&mut out, &mut text)?;
                    self.push(
                        &mut out,
                        Node::Number(plural.unwrap().into(), NumberStyle::Decimal),
                    )?;
                }
                '\'' => self.quoted(&mut text, plural.is_some()),
                '<' => {
                    let tail = &self.rest()[1..];
                    let is_close = tail.starts_with('/');
                    let digits = tail.strip_prefix('/').unwrap_or(tail);
                    if digits.starts_with(|c: char| c.is_ascii_digit()) {
                        self.flush(&mut out, &mut text)?;
                        self.bump();
                        if is_close {
                            self.bump();
                        }
                        let start = self.offset;
                        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                            self.bump();
                        }
                        let index = self.source[start..self.offset]
                            .parse::<u16>()
                            .map_err(|_| self.error("element index exceeds 65535"))?;
                        self.expect('>')?;
                        if is_close {
                            if end_element != Some(index) {
                                return Err(self.error("mismatched closing element placeholder"));
                            }
                            return Ok(out);
                        }
                        if elements >= MAX_ELEMENT_DEPTH {
                            return Err(self.error("element nesting exceeds 16 levels"));
                        }
                        let children = self.sequence(Some(index), plural, choices, elements + 1)?;
                        self.push(&mut out, Node::Element(index, children))?;
                    } else if digits.starts_with(|c: char| c.is_ascii_alphabetic())
                        || tail.starts_with('!')
                    {
                        return Err(self.error("markup belongs in the template; use positional <0>...</0> placeholders"));
                    } else {
                        text.push(c);
                        self.bump();
                    }
                }
                _ => {
                    text.push(c);
                    self.bump();
                }
            }
        }
        if end_element.is_some() {
            return Err(self.error("unclosed element placeholder"));
        }
        self.flush(&mut out, &mut text)?;
        Ok(out)
    }

    fn quoted(&mut self, text: &mut String, plural: bool) {
        self.bump();
        if self.peek() == Some('\'') {
            self.bump();
            text.push('\'');
            return;
        }
        if !self
            .peek()
            .is_some_and(|c| matches!(c, '{' | '}') || (plural && c == '#') || c == '<')
        {
            text.push('\'');
            return;
        }
        // ICU apostrophe-friendly mode: a quote only starts before syntax;
        // doubled quotes escape themselves, and an open quote closes at EOF.
        while let Some(c) = self.bump() {
            if c == '\'' {
                if self.peek() == Some('\'') {
                    self.bump();
                    text.push('\'');
                } else {
                    return;
                }
            } else {
                text.push(c);
            }
        }
    }

    fn placeholder(
        &mut self,
        parent_plural: Option<&str>,
        choices: usize,
        elements: usize,
    ) -> Result<Node, MessageError> {
        self.expect('{')?;
        self.whitespace();
        let name = self.identifier()?;
        self.whitespace();
        if self.peek() == Some('}') {
            self.bump();
            self.argument(&name, None)?;
            return Ok(Node::Argument(name));
        }
        self.expect(',')?;
        self.whitespace();
        let kind = self.identifier()?;
        self.whitespace();
        match kind.as_str() {
            "number" | "date" | "time" => {
                let style = if self.peek() == Some(',') {
                    self.bump();
                    self.whitespace();
                    let style = self.identifier()?;
                    self.whitespace();
                    Some(style)
                } else {
                    None
                };
                self.expect('}')?;
                if kind == "number" {
                    self.argument(&name, Some(ArgumentKind::Number))?;
                    let style = match style.as_deref() {
                        None => NumberStyle::Decimal,
                        Some("percent") => NumberStyle::Percent,
                        _ => {
                            return Err(
                                self.error("number supports only the default or percent style")
                            );
                        }
                    };
                    Ok(Node::Number(name, style))
                } else {
                    self.argument(&name, Some(ArgumentKind::DateTime))?;
                    let length = match style.as_deref() {
                        None | Some("medium") => StyleLength::Medium,
                        Some("short") => StyleLength::Short,
                        Some("long") if kind == "date" => StyleLength::Long,
                        _ => return Err(self.error("unsupported date/time style")),
                    };
                    Ok(Node::DateTime(
                        name,
                        if kind == "date" {
                            DateTimeStyle::Date(length)
                        } else {
                            DateTimeStyle::Time(length)
                        },
                    ))
                }
            }
            "plural" | "select" => {
                if choices >= MAX_CHOICE_DEPTH {
                    return Err(self.error("plural/select nesting exceeds one nested level"));
                }
                let plural = kind == "plural";
                self.argument(
                    &name,
                    Some(if plural {
                        ArgumentKind::Number
                    } else {
                        ArgumentKind::Text
                    }),
                )?;
                self.expect(',')?;
                self.whitespace();
                let mut variants: Vec<(Selector, Vec<Node>)> = Vec::new();
                while self.peek() != Some('}') {
                    let selector = if self.peek() == Some('=') && plural {
                        self.bump();
                        let start = self.offset;
                        while self
                            .peek()
                            .is_some_and(|c| c.is_ascii_digit() || matches!(c, '-' | '.'))
                        {
                            self.bump();
                        }
                        Selector::Exact(
                            self.source[start..self.offset]
                                .parse()
                                .map_err(|_| self.error("invalid exact plural selector"))?,
                        )
                    } else {
                        let value = self.identifier()?;
                        if plural
                            && !["zero", "one", "two", "few", "many", "other"]
                                .contains(&value.as_str())
                        {
                            return Err(self.error(
                                "unknown plural category; offsets and ordinals are not supported",
                            ));
                        }
                        Selector::Named(value)
                    };
                    let duplicate =
                        variants
                            .iter()
                            .any(|(previous, _)| match (&selector, previous) {
                                (Selector::Named(a), Selector::Named(b)) => a == b,
                                (Selector::Exact(a), Selector::Exact(b)) => a.numeric_eq(*b),
                                _ => false,
                            });
                    if duplicate {
                        return Err(self.error("duplicate message selector"));
                    }
                    self.whitespace();
                    self.expect('{')?;
                    let nodes = self.sequence(
                        None,
                        if plural { Some(&name) } else { parent_plural },
                        choices + 1,
                        elements,
                    )?;
                    self.expect('}')?;
                    self.whitespace();
                    variants.push((selector, nodes));
                }
                self.expect('}')?;
                if !variants
                    .iter()
                    .any(|(s, _)| matches!(s, Selector::Named(n) if n == "other"))
                {
                    return Err(self.error("plural/select requires an other branch"));
                }
                Ok(Node::Choice {
                    name,
                    plural,
                    variants,
                })
            }
            "selectordinal" => Err(self
                .error("selectordinal is not supported; cardinal and ordinal rules are distinct")),
            _ => Err(self.error(format!("unsupported message formatter {kind}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(parts: &[MessagePart<'_>]) -> String {
        parts
            .iter()
            .map(|part| match part {
                MessagePart::Text(s) => s.to_string(),
                MessagePart::Number { value, .. } => value.to_string(),
                other => panic!("unexpected {other:?}"),
            })
            .collect()
    }

    #[test]
    fn interpolation_and_apostrophe_friendly_escaping() {
        let message = Message::parse("L'utilisateur {name}: '{'ok'}' and ''quotes'' #").unwrap();
        let args = [("name", Value::Text("Zoë <script>"))];
        assert_eq!(
            text(&message.parts(&"fr".parse().unwrap(), &args).unwrap()),
            "L'utilisateur Zoë <script>: {ok} and 'quotes' #"
        );
        assert!(matches!(
            message.parts(&"en".parse().unwrap(), &[]),
            Err(FormatError::MissingArgument(_))
        ));
    }

    #[test]
    fn exact_selectors_win_and_pound_tracks_the_nearest_plural() {
        let message = Message::parse("{n, plural, one {one} other {{gender, select, she {she has #} other {they have #}}} =1.0 {exact}}").unwrap();
        let args = [
            ("n", Value::Number("1.00".parse().unwrap())),
            ("gender", Value::Text("she")),
        ];
        assert_eq!(
            text(&message.parts(&"en".parse().unwrap(), &args).unwrap()),
            "exact"
        );
        let args = [
            ("n", Value::Number(2u64.into())),
            ("gender", Value::Text("she")),
        ];
        assert_eq!(
            text(&message.parts(&"en".parse().unwrap(), &args).unwrap()),
            "she has 2"
        );
    }

    #[test]
    fn typed_format_requests_and_element_reordering_preserve_structure() {
        let message = Message::parse("<1>Privacy</1>, <0>terms</0>: {n, number, percent}; {at, date, long}; {at, time, short}").unwrap();
        assert_eq!(message.arguments()["at"], ArgumentKind::DateTime);
        assert_eq!(message.elements(), &BTreeSet::from([0, 1]));
        let at = DateTimeArg::new(0, crate::TimeZone::utc()).unwrap();
        let args = [
            ("n", Value::Number("0.5".parse().unwrap())),
            ("at", Value::DateTime(&at)),
        ];
        let parts = message.parts(&"fr".parse().unwrap(), &args).unwrap();
        assert_eq!(parts[0], MessagePart::OpenElement(1));
        assert!(parts.contains(&MessagePart::Number {
            value: "0.5".parse().unwrap(),
            style: NumberStyle::Percent
        }));
        assert!(parts.contains(&MessagePart::DateTime {
            value: &at,
            style: DateTimeStyle::Date(StyleLength::Long)
        }));
    }

    #[test]
    fn rejects_malformed_and_out_of_subset_messages_without_partial_output() {
        for source in [
            "{",
            "}",
            "{x",
            "{x, plural, one {only}}",
            "{x, select, other {a} other {b}}",
            "{x, plural, =1 {a} =1.0 {b} other {c}}",
            "{x, selectordinal, one {a} other {b}}",
            "{x, number, currency}",
            "{x, plural, offset:1 other {#}}",
            "{x, number} {x, select, other {bad}}",
            "{x, time, long}",
            "<b>text</b>",
            "<0>text</1>",
            "<0>text",
            "<0>one</0><0>two</0>",
            "{x, select, one {<0>a</0>} other {none}}",
            "{a, select, other {{b, select, other {{c, select, other {bad}}}}}}",
        ] {
            assert!(Message::parse(source).is_err(), "accepted {source}");
        }
        assert!(Message::parse(&"x".repeat(MAX_MESSAGE_BYTES + 1)).is_err());
    }

    #[test]
    fn validates_arguments_even_in_unselected_branches() {
        let message =
            Message::parse("{state, select, ready {Done} other {Wait {seconds, number}}}").unwrap();
        let locale = "en".parse().unwrap();
        let args = [("state", Value::Text("ready"))];
        assert!(matches!(
            message.parts(&locale, &args),
            Err(FormatError::MissingArgument(_))
        ));
        let args = [
            ("state", Value::Text("ready")),
            ("seconds", Value::Text("2")),
        ];
        assert!(matches!(
            message.parts(&locale, &args),
            Err(FormatError::ArgumentType { .. })
        ));
    }
}
