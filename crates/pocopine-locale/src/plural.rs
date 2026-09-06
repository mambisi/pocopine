use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::Locale;

/// Cardinal categories defined by CLDR. Ordinals are deliberately separate
/// from this API and are not in RFC-120's initial message subset.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluralCategory {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

impl PluralCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::Two => "two",
            Self::Few => "few",
            Self::Many => "many",
            Self::Other => "other",
        }
    }
}

/// Exact decimal input retaining visible fractional zeros. `1`, `1.0`, and
/// `1.00` need not select the same plural form. Floating-point conversion is
/// intentionally absent: it cannot recover that information.
///
/// Supports a u64 integer magnitude and up to 19 decimal places. Values beyond
/// these limits are rejected, never rounded. Negative values select categories
/// by magnitude, while formatting and exact-message selectors retain the sign.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PluralArg {
    negative: bool,
    pub(crate) i: u64,
    pub(crate) v: u8,
    pub(crate) f: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidPluralArg;

impl fmt::Display for InvalidPluralArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected an exact decimal with a u64 magnitude and at most 19 fraction digits")
    }
}
impl std::error::Error for InvalidPluralArg {}

impl PluralArg {
    pub fn new(
        negative: bool,
        integer: u64,
        visible_fraction_digits: u8,
        fraction: u64,
    ) -> Result<Self, InvalidPluralArg> {
        if visible_fraction_digits > 19 || fraction >= 10u64.pow(visible_fraction_digits.into()) {
            return Err(InvalidPluralArg);
        }
        Ok(Self {
            negative,
            i: integer,
            v: visible_fraction_digits,
            f: fraction,
        })
    }

    pub fn integer(self) -> u64 {
        self.i
    }
    pub fn visible_fraction_digits(self) -> u8 {
        self.v
    }
    pub fn fraction(self) -> u64 {
        self.f
    }
    pub fn is_negative(self) -> bool {
        self.negative
    }

    pub(crate) fn t(self) -> u64 {
        let mut fraction = self.f;
        while fraction != 0 && fraction.is_multiple_of(10) {
            fraction /= 10;
        }
        fraction
    }

    /// Numeric comparison for an MF1 exact selector; unlike plural-category
    /// selection this ignores visible trailing zeros and preserves the sign.
    pub fn numeric_eq(self, other: Self) -> bool {
        let zero = self.i == 0 && self.f == 0 && other.i == 0 && other.f == 0;
        if !zero && self.negative != other.negative {
            return false;
        }
        let normalized = |value: Self| {
            let (mut fraction, mut digits) = (value.f, value.v);
            while digits > 0 && fraction.is_multiple_of(10) {
                fraction /= 10;
                digits -= 1;
            }
            (value.i, fraction, digits)
        };
        normalized(self) == normalized(other)
    }
}

impl FromStr for PluralArg {
    type Err = InvalidPluralArg;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > 41 {
            return Err(InvalidPluralArg);
        }
        let (negative, value) = value
            .strip_prefix('-')
            .map_or((false, value), |s| (true, s));
        let (integer, fraction) = value
            .split_once('.')
            .map_or((value, None), |(i, f)| (i, Some(f)));
        if integer.is_empty() || !integer.bytes().all(|b| b.is_ascii_digit()) {
            return Err(InvalidPluralArg);
        }
        let i = integer.parse().map_err(|_| InvalidPluralArg)?;
        let (v, f) = match fraction {
            None => (0, 0),
            Some(f) if !f.is_empty() && f.len() <= 19 && f.bytes().all(|b| b.is_ascii_digit()) => {
                (f.len() as u8, f.parse().map_err(|_| InvalidPluralArg)?)
            }
            _ => return Err(InvalidPluralArg),
        };
        Self::new(negative, i, v, f)
    }
}

impl From<u64> for PluralArg {
    fn from(value: u64) -> Self {
        Self {
            negative: false,
            i: value,
            v: 0,
            f: 0,
        }
    }
}
impl From<i64> for PluralArg {
    fn from(value: i64) -> Self {
        Self {
            negative: value < 0,
            i: value.unsigned_abs(),
            v: 0,
            f: 0,
        }
    }
}
impl TryFrom<String> for PluralArg {
    type Error = InvalidPluralArg;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
impl From<PluralArg> for String {
    fn from(value: PluralArg) -> Self {
        value.to_string()
    }
}
impl fmt::Display for PluralArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.negative {
            f.write_str("-")?;
        }
        write!(f, "{}", self.i)?;
        if self.v > 0 {
            write!(f, ".{:0width$}", self.f, width = self.v as usize)?;
        }
        Ok(())
    }
}

/// One generated CLDR predicate set. Resolve once per locale, then reuse it
/// across messages. Its index is internal to this library version, not a
/// durable identifier or an application error code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CardinalRule(u8);

impl CardinalRule {
    pub fn for_locale(locale: &Locale) -> Self {
        // CLDR's plural component has its own (empty in this snapshot)
        // parent override table. Text fallback such as sr-Latn -> root or
        // hi-Latn -> en-IN must not change the language's plural grammar.
        let mut next = Some(locale.as_str());
        while let Some(candidate) = next {
            if let Some(rule) = crate::generated::rule_for_tag(candidate) {
                return Self(rule);
            }
            next = candidate.rsplit_once('-').map(|(parent, _)| parent);
        }
        Self(crate::generated::OTHER_RULE)
    }

    pub fn category(self, value: PluralArg) -> PluralCategory {
        crate::generated::category(self.0, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_operands_preserve_zeroes_sign_and_integer_boundaries() {
        for value in [
            "1",
            "1.0",
            "1.00",
            "0.0010",
            "-0.0",
            "-9223372036854775808",
            "18446744073709551615.9999999999999999999",
        ] {
            let parsed: PluralArg = value.parse().unwrap();
            assert_eq!(parsed.to_string(), value);
            assert_eq!(
                serde_json::from_str::<PluralArg>(&serde_json::to_string(&parsed).unwrap())
                    .unwrap(),
                parsed
            );
        }
        assert_eq!(PluralArg::from(i64::MIN).to_string(), i64::MIN.to_string());
        for value in [
            "NaN",
            "inf",
            "1e6",
            "1.",
            ".1",
            "+1",
            "18446744073709551616",
            "0.00000000000000000000",
        ] {
            assert!(value.parse::<PluralArg>().is_err(), "{value}");
        }
        assert!("1.00".parse::<PluralArg>().unwrap().numeric_eq(1u64.into()));
        assert!(!"-1".parse::<PluralArg>().unwrap().numeric_eq(1u64.into()));
    }

    #[test]
    fn fractional_categories_differ_from_numeric_exact_matches() {
        let rule = |locale: &str, value: &str| {
            CardinalRule::for_locale(&locale.parse().unwrap()).category(value.parse().unwrap())
        };
        assert_eq!(rule("en", "1.0"), PluralCategory::Other);
        assert_eq!(rule("es", "1.0"), PluralCategory::One);
        assert_eq!(rule("ar", "3.5"), PluralCategory::Other);
        assert_eq!(rule("ar", "3.0"), PluralCategory::Few);
        assert_eq!(rule("ru", "22"), PluralCategory::Few);
        assert_eq!(rule("ja", "1"), PluralCategory::Other);
        assert_eq!(rule("sr-Latn", "2"), PluralCategory::Few);
        assert_eq!(rule("hi-Latn", "0"), PluralCategory::One);
    }
}
