use std::{collections::BTreeSet, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// A canonical Unicode language identifier, such as `fr-CA` or `zh-Hant`.
///
/// Accepts language, optional script/region, and variant subtags. Unicode
/// extensions and private-use tags are formatting preferences, not catalog
/// identities, and are rejected. Parsing does not select an application locale;
/// use [`Locales::resolve`] against the configured set for that.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Locale(String);

/// A malformed language identifier or inconsistent configured locale set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidLocale(pub String);

impl fmt::Display for InvalidLocale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for InvalidLocale {}

impl Locale {
    pub fn parse(value: &str) -> Result<Self, InvalidLocale> {
        value.parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn language(&self) -> &str {
        self.0.split('-').next().expect("validated language")
    }

    /// CLDR's explicit parent, otherwise ordinary subtag truncation.
    /// `und` denotes CLDR root and has no parent.
    pub fn parent(&self) -> Option<Self> {
        if self.0 == "und" {
            return None;
        }
        if let Some(parent) = crate::generated::parent(&self.0) {
            return Some(Self(parent.to_owned()));
        }
        self.0
            .rsplit_once('-')
            .map(|(parent, _)| Self(parent.into()))
    }
}

impl FromStr for Locale {
    type Err = InvalidLocale;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidLocale(format!("invalid locale identifier: {value:?}"));
        if value.len() > 128 || !value.is_ascii() {
            return Err(invalid());
        }
        let mut parts = value.split('-').peekable();
        let language = parts.next().ok_or_else(invalid)?;
        if !(2..=8).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_alphabetic())
        {
            return Err(invalid());
        }
        let mut out = language.to_ascii_lowercase();
        if let Some(&script) = parts.peek()
            && script.len() == 4
            && script.bytes().all(|b| b.is_ascii_alphabetic())
        {
            out.push('-');
            out.push_str(&script[..1].to_ascii_uppercase());
            out.push_str(&script[1..].to_ascii_lowercase());
            parts.next();
        }
        if let Some(&region) = parts.peek()
            && ((region.len() == 2 && region.bytes().all(|b| b.is_ascii_alphabetic()))
                || (region.len() == 3 && region.bytes().all(|b| b.is_ascii_digit())))
        {
            out.push('-');
            out.push_str(&region.to_ascii_uppercase());
            parts.next();
        }
        let mut seen = BTreeSet::new();
        for part in parts {
            if !((5..=8).contains(&part.len())
                || (part.len() == 4 && part.as_bytes()[0].is_ascii_digit()))
                || !part.bytes().all(|b| b.is_ascii_alphanumeric())
                || !seen.insert(part.to_ascii_lowercase())
            {
                return Err(invalid());
            }
            out.push('-');
            out.push_str(&part.to_ascii_lowercase());
        }
        Ok(Self(out))
    }
}

impl TryFrom<String> for Locale {
    type Error = InvalidLocale;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Locale> for String {
    fn from(value: Locale) -> Self {
        value.0
    }
}

impl fmt::Display for Locale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Validated application locale set. Fallback is deterministic and shared by
/// the catalog compiler, request negotiation, and recipient-message workers.
#[derive(Clone, Debug)]
pub struct Locales {
    default: Locale,
    supported: BTreeSet<Locale>,
}

impl Locales {
    pub fn new(
        default: Locale,
        supported: impl IntoIterator<Item = Locale>,
    ) -> Result<Self, InvalidLocale> {
        let mut set = BTreeSet::new();
        for locale in supported {
            if !set.insert(locale.clone()) {
                return Err(InvalidLocale(format!(
                    "duplicate configured locale: {locale}"
                )));
            }
        }
        if !set.contains(&default) {
            return Err(InvalidLocale(format!(
                "default locale {default} is not configured"
            )));
        }
        Ok(Self {
            default,
            supported: set,
        })
    }

    pub fn default_locale(&self) -> &Locale {
        &self.default
    }

    pub fn supported(&self) -> impl Iterator<Item = &Locale> {
        self.supported.iter()
    }

    /// Supported entries in the CLDR parent chain, followed by the default.
    /// Message fallback uses the same chain as language negotiation.
    pub fn fallback_chain(&self, requested: &Locale) -> Vec<&Locale> {
        let mut result = Vec::new();
        let mut next = Some(requested.clone());
        while let Some(locale) = next {
            if let Some(configured) = self.supported.get(&locale) {
                result.push(configured);
            }
            next = locale.parent();
        }
        if !result.contains(&&self.default) {
            result.push(&self.default);
        }
        result
    }

    pub fn resolve(&self, requested: &Locale) -> &Locale {
        self.fallback_chain(requested)[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_canonicalizes_identifiers_at_deserialization_too() {
        for (input, expected) in [
            ("FR-ca", "fr-CA"),
            ("zh-hANT-tw", "zh-Hant-TW"),
            ("es-419", "es-419"),
            ("sl-rozaj-biske", "sl-rozaj-biske"),
        ] {
            assert_eq!(Locale::parse(input).unwrap().as_str(), expected);
        }
        for bad in [
            "",
            "e",
            "en_US",
            "en--US",
            "en/US",
            "en-u-nu-arab",
            "x-private",
            "en-variant-variant",
            "éé",
            "en-1234-",
        ] {
            assert!(Locale::parse(bad).is_err(), "{bad}");
            assert!(serde_json::from_str::<Locale>(&serde_json::to_string(bad).unwrap()).is_err());
        }
    }

    #[test]
    fn parents_precede_default_and_obey_script_boundaries() {
        let locales = Locales::new(
            "en".parse().unwrap(),
            ["en", "fr", "es", "es-419", "zh"].map(|s| s.parse().unwrap()),
        )
        .unwrap();
        assert_eq!(locales.resolve(&"fr-CA".parse().unwrap()).as_str(), "fr");
        assert_eq!(
            locales.resolve(&"es-AR".parse().unwrap()).as_str(),
            "es-419"
        );
        assert_eq!(locales.resolve(&"zh-Hant".parse().unwrap()).as_str(), "en");
        assert_eq!(locales.resolve(&"xx".parse().unwrap()).as_str(), "en");
    }
}
