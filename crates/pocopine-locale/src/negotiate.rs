use crate::{Locale, Locales};

/// Explicit committed UI locale on a server-function request. It controls
/// presentation only; callers must never use it as an authorization input.
pub const LOCALE_HEADER: &str = "pocopine-locale";
pub const LOCALE_COOKIE: &str = "pocopine_locale";

/// Ordered inputs to boundary negotiation. `route` is a locale segment already
/// recognized by the configured router, not an arbitrary first path segment.
/// A server-function endpoint normally has no route locale and uses `explicit`.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalePreferences<'a> {
    pub route: Option<&'a str>,
    pub explicit: Option<&'a str>,
    pub cookie: Option<&'a str>,
    /// Combined Accept-Language field, or an ordered comma-separated list of
    /// browser languages. Quality weights use HTTP's integer thousandths.
    pub accepted: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocaleSource {
    Route,
    Explicit,
    Cookie,
    Accepted,
    Default,
}

/// The supported locale to snapshot in a request, stream or recipient job.
/// Source information lets a router distinguish an explicit selection from
/// first-visit language detection without re-running negotiation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NegotiatedLocale {
    pub locale: Locale,
    pub source: LocaleSource,
}

impl Locales {
    /// Resolve explicit inputs before passive language detection. Each input
    /// tries the same CLDR parent chain as catalog fallback, without jumping
    /// to the configured default until all preferences have been considered.
    /// Malformed and unsupported preferences are skipped.
    ///
    /// Accept-Language weights follow RFC 9110 section 12.4.2. Equal weights
    /// retain source order. Zero-weight language ranges exclude their matching
    /// tags from passive detection; they cannot override an explicit choice.
    /// When no acceptable supported locale exists, the application deliberately
    /// uses its configured default instead of returning HTTP 406.
    pub fn negotiate(&self, preferences: LocalePreferences<'_>) -> NegotiatedLocale {
        for (input, source) in [
            (preferences.route, LocaleSource::Route),
            (preferences.explicit, LocaleSource::Explicit),
            (preferences.cookie, LocaleSource::Cookie),
        ] {
            if let Some(locale) = input.and_then(|value| value.parse::<Locale>().ok())
                && let Some(locale) = self.matching(&locale)
            {
                return NegotiatedLocale {
                    locale: locale.clone(),
                    source,
                };
            }
        }
        let mut accepted = accepted(preferences.accepted);
        // Stable sorting is intentional: browser language order breaks ties.
        accepted.sort_by_key(|entry| std::cmp::Reverse(entry.weight));
        for entry in accepted.iter().filter(|entry| entry.weight > 0) {
            let candidate = if let Some(requested) = &entry.locale {
                self.matching(requested)
                    .filter(|candidate| !excluded(candidate, &accepted, true))
            } else {
                std::iter::once(self.default_locale())
                    .chain(self.supported())
                    .find(|candidate| !excluded(candidate, &accepted, false))
            };
            if let Some(locale) = candidate {
                return NegotiatedLocale {
                    locale: locale.clone(),
                    source: LocaleSource::Accepted,
                };
            }
        }
        NegotiatedLocale {
            locale: self.default_locale().clone(),
            source: LocaleSource::Default,
        }
    }

    /// A configured locale in the requested CLDR chain, with no default added.
    /// Useful when trying several preferences before selecting a fallback.
    pub fn matching(&self, requested: &Locale) -> Option<&Locale> {
        let mut candidate = Some(requested.clone());
        while let Some(locale) = candidate {
            if let Some(configured) = self.supported().find(|value| **value == locale) {
                return Some(configured);
            }
            candidate = locale.parent();
        }
        None
    }
}

struct Accepted {
    locale: Option<Locale>,
    weight: u16,
}

fn accepted(header: &str) -> Vec<Accepted> {
    // Bound work before parsing; never silently drop a late q=0 exclusion.
    if header.len() > 8192 || header.split(',').count() > 64 {
        return vec![];
    }
    header
        .split(',')
        .filter_map(|entry| {
            let mut parts = entry.split(';');
            let name = parts.next()?.trim_matches([' ', '\t']);
            let locale = if name == "*" {
                None
            } else {
                Some(name.parse().ok()?)
            };
            let weight = match parts.next() {
                Some(parameter) => {
                    let (name, value) = parameter.trim_matches([' ', '\t']).split_once('=')?;
                    if !name.eq_ignore_ascii_case("q") || parts.next().is_some() {
                        return None;
                    }
                    quality(value)?
                }
                None => 1000,
            };
            Some(Accepted { locale, weight })
        })
        .collect()
}

fn quality(value: &str) -> Option<u16> {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match whole {
        "1" if fraction.bytes().all(|b| b == b'0') => Some(1000),
        "0" => Some(
            fraction
                .bytes()
                .fold(0u16, |n, b| n * 10 + u16::from(b - b'0'))
                * 10u16.pow(3 - fraction.len() as u32),
        ),
        _ => None,
    }
}

fn excluded(locale: &Locale, accepted: &[Accepted], specific: bool) -> bool {
    accepted.iter().any(|entry| {
        entry.weight == 0
            && match &entry.locale {
                Some(range) => {
                    locale == range
                        || locale
                            .as_str()
                            .strip_prefix(range.as_str())
                            .is_some_and(|tail| tail.starts_with('-'))
                }
                None => !specific,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn locales() -> Locales {
        Locales::new(
            "en".parse().unwrap(),
            ["en", "fr", "de", "zh", "zh-Hant", "es-419"].map(|s| s.parse().unwrap()),
        )
        .unwrap()
    }
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn explicit_choices_precede_passive_detection_and_skip_unsupported_inputs() {
        let locales = locales();
        let mut input = LocalePreferences {
            route: Some("de"),
            explicit: Some("fr-CA"),
            cookie: Some("zh-Hant"),
            accepted: "en",
        };
        assert_eq!(
            locales.negotiate(input),
            NegotiatedLocale {
                locale: "de".parse().unwrap(),
                source: LocaleSource::Route
            }
        );
        input.route = None;
        assert_eq!(locales.negotiate(input).locale.as_str(), "fr");
        assert_eq!(locales.negotiate(input).source, LocaleSource::Explicit);
        input.explicit = Some("ja");
        assert_eq!(locales.negotiate(input).source, LocaleSource::Cookie);
        input.cookie = Some("../en");
        assert_eq!(locales.negotiate(input).source, LocaleSource::Accepted);
        input.accepted = "ja, xx";
        assert_eq!(locales.negotiate(input).source, LocaleSource::Default);
    }
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn weights_parent_chains_exclusions_and_order_are_deterministic() {
        for (header, expected) in [
            ("ja, fr-CA;q=0.9, de;q=0.8", "fr"),
            ("de;q=0.8,fr;q=0.8", "de"),
            ("fr;q=0.8,de;q=0.8", "fr"),
            ("fr;q=0,*;q=0.9,en;q=0.1", "en"),
            ("*;q=0,fr;q=1", "fr"),
            ("fr;q=0,fr-CA;q=1,de;q=0.5", "de"),
            ("es-AR", "es-419"),
            ("zh-Hant-TW", "zh-Hant"),
            ("fr;q=NaN,de;q=1", "de"),
            ("fr;q=0.0001,de;q=1", "de"),
            ("fr;q=1.001,de;q=1", "de"),
            ("fr;q=1;q=0,de;q=1", "de"),
            ("en;q=0,*;q=0", "en"),
            ("fr;Q=1.000", "fr"),
        ] {
            assert_eq!(
                locales()
                    .negotiate(LocalePreferences {
                        accepted: header,
                        ..Default::default()
                    })
                    .locale
                    .as_str(),
                expected,
                "{header}"
            );
        }
        assert_eq!(
            locales()
                .negotiate(LocalePreferences {
                    explicit: Some("fr"),
                    accepted: "fr;q=0",
                    ..Default::default()
                })
                .locale
                .as_str(),
            "fr"
        );
        let simplified = Locales::new(
            "en".parse().unwrap(),
            ["en", "zh"].map(|s| s.parse().unwrap()),
        )
        .unwrap();
        assert!(
            simplified
                .matching(&"zh-Hant-TW".parse().unwrap())
                .is_none()
        );
    }
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
    #[cfg_attr(not(target_arch = "wasm32"), test)]
    fn oversized_detection_is_ignored_as_a_whole() {
        for header in [
            format!("fr,{}", " ".repeat(8192)),
            format!("{}fr;q=0", "fr,".repeat(64)),
        ] {
            assert_eq!(
                locales()
                    .negotiate(LocalePreferences {
                        accepted: &header,
                        ..Default::default()
                    })
                    .source,
                LocaleSource::Default
            );
        }
    }
}
