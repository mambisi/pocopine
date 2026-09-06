use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{DateTimeStyle, Locale, MessagePart, StyleLength, platform};

/// An explicit recipient timezone. It never reads the process/browser's
/// current zone. ICU rendering uses bundled IANA data; browser Intl uses the
/// browser's own IANA data unless strict-parity is enabled.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TimeZone(String);

impl TimeZone {
    pub fn parse(name: &str) -> Result<Self, RenderError> {
        if name.is_empty()
            || name.len() > 128
            || !name.starts_with(|c: char| c.is_ascii_alphabetic())
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_+-/".contains(&b))
        {
            return Err(RenderError::InvalidTimeZone(name.into()));
        }
        platform::validate_time_zone(name)?;
        Ok(Self(name.into()))
    }
    pub fn utc() -> Self {
        Self("UTC".into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for TimeZone {
    type Error = RenderError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}
impl From<TimeZone> for String {
    fn from(value: TimeZone) -> Self {
        value.0
    }
}

/// A timestamp with an explicit timezone for date/time messages. Store both
/// in recipient jobs so retries retain the intended timezone and instant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "DateTimeInput", into = "DateTimeInput")]
pub struct DateTimeArg {
    unix_millis: i64,
    time_zone: TimeZone,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DateTimeInput {
    unix_millis: i64,
    time_zone: TimeZone,
}

impl DateTimeArg {
    // Shared with the bounded Jiff instant range. Leaving room for every
    // legal UTC offset keeps converted civil dates within -9999..=9999.
    pub const MIN_UNIX_MILLIS: i64 = -377_705_023_201_000;
    pub const MAX_UNIX_MILLIS: i64 = 253_402_207_200_000;

    pub fn new(unix_millis: i64, time_zone: TimeZone) -> Result<Self, RenderError> {
        if !(Self::MIN_UNIX_MILLIS..=Self::MAX_UNIX_MILLIS).contains(&unix_millis) {
            return Err(RenderError::InvalidTimestamp(unix_millis));
        }
        Ok(Self {
            unix_millis,
            time_zone,
        })
    }
    pub fn unix_millis(&self) -> i64 {
        self.unix_millis
    }
    pub fn time_zone(&self) -> &TimeZone {
        &self.time_zone
    }
}
impl TryFrom<DateTimeInput> for DateTimeArg {
    type Error = RenderError;
    fn try_from(value: DateTimeInput) -> Result<Self, Self::Error> {
        Self::new(value.unix_millis, value.time_zone)
    }
}
impl From<DateTimeArg> for DateTimeInput {
    fn from(value: DateTimeArg) -> Self {
        Self {
            unix_millis: value.unix_millis,
            time_zone: value.time_zone,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderError {
    InvalidTimeZone(String),
    InvalidTimestamp(i64),
    Formatter(String),
    ElementPlaceholdersRequireTemplate,
}
impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTimeZone(name) => write!(f, "invalid IANA time zone {name:?}"),
            Self::InvalidTimestamp(value) => write!(
                f,
                "timestamp {value} is outside the supported -9999..9999 civil-date range"
            ),
            Self::Formatter(message) => write!(f, "locale formatting failed: {message}"),
            Self::ElementPlaceholdersRequireTemplate => {
                f.write_str("element placeholders require a template renderer")
            }
        }
    }
}
impl std::error::Error for RenderError {}

/// Formatted text plus stable markers for template-owned elements. Translated
/// markup is never parsed or inserted as HTML.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RenderedPart {
    Text(String),
    OpenElement(u16),
    CloseElement(u16),
}

/// Per-locale text formatting, prepared before requests/DOM updates. This is
/// the fixed platform policy from RFC-120, not a pluggable backend interface.
pub struct MessageFormatter {
    locale: Locale,
    formatter: platform::Formatter,
}

impl MessageFormatter {
    pub fn new(locale: Locale) -> Result<Self, RenderError> {
        let formatter = platform::Formatter::new(&locale)?;
        Ok(Self { locale, formatter })
    }
    pub fn locale(&self) -> &Locale {
        &self.locale
    }

    pub fn render(&self, parts: &[MessagePart<'_>]) -> Result<Vec<RenderedPart>, RenderError> {
        parts
            .iter()
            .map(|part| {
                Ok(match part {
                    MessagePart::Text(text) => RenderedPart::Text(text.to_string()),
                    MessagePart::Number { value, style } => {
                        RenderedPart::Text(self.formatter.number(*value, *style)?)
                    }
                    MessagePart::DateTime { value, style } => {
                        if *style == DateTimeStyle::Time(StyleLength::Long) {
                            return Err(RenderError::Formatter(
                                "long time style is outside the MF1 subset".into(),
                            ));
                        }
                        RenderedPart::Text(self.formatter.datetime(value, *style)?)
                    }
                    MessagePart::OpenElement(id) => RenderedPart::OpenElement(*id),
                    MessagePart::CloseElement(id) => RenderedPart::CloseElement(*id),
                })
            })
            .collect()
    }

    pub fn format(&self, parts: &[MessagePart<'_>]) -> Result<String, RenderError> {
        if parts.iter().any(|part| {
            matches!(
                part,
                MessagePart::OpenElement(_) | MessagePart::CloseElement(_)
            )
        }) {
            return Err(RenderError::ElementPlaceholdersRequireTemplate);
        }
        let rendered = self.render(parts)?;
        let mut text = String::new();
        for part in rendered {
            if let RenderedPart::Text(value) = part {
                text.push_str(&value);
            }
        }
        Ok(text)
    }
}
