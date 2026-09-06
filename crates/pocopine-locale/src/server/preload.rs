use std::collections::BTreeMap;

use crate::{Locale, Locales};

/// Resolve script direction at build time; the browser receives one value per
/// configured locale and does not need ICU data for document directionality.
pub fn locale_directions(locales: &Locales) -> BTreeMap<Locale, crate::TextDirection> {
    let directionality = icu_locale::LocaleDirectionality::new_extended();
    locales
        .supported()
        .map(|locale| {
            let id = locale
                .as_str()
                .parse()
                .expect("validated Unicode language identifier");
            let direction = if directionality.get(&id) == Some(icu_locale::Direction::RightToLeft) {
                crate::TextDirection::Rtl
            } else {
                crate::TextDirection::Ltr
            };
            (locale.clone(), direction)
        })
        .collect()
}

/// Resolve CLDR's exceptional parent boundaries for the tiny pre-wasm loader.
/// Ordinary subtag truncation is sufficient between these boundaries. `None`
/// is significant: e.g. zh-Hant must not fall through to configured zh.
pub fn preload_fallbacks(locales: &Locales) -> BTreeMap<Locale, Option<Locale>> {
    let data: serde_json::Value =
        serde_json::from_str(include_str!("../../data/cldr/parentLocales.json"))
            .expect("vendored CLDR JSON");
    data["supplemental"]["parentLocales"]["parentLocale"]
        .as_object()
        .expect("vendored CLDR parent map")
        .keys()
        .map(|tag| {
            let locale: Locale = tag.parse().expect("vendored CLDR locale");
            let matched = locales.matching(&locale).cloned();
            (locale, matched)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_uses_the_script_and_preloads_preserve_cldr_boundaries() {
        let locales = Locales::new(
            "en".parse().unwrap(),
            ["en", "ar", "ar-Latn", "az-Arab", "zh", "es-419"].map(|s| s.parse().unwrap()),
        )
        .unwrap();
        let directions = locale_directions(&locales);
        for (tag, expected) in [
            ("en", "ltr"),
            ("ar", "rtl"),
            ("ar-Latn", "ltr"),
            ("az-Arab", "rtl"),
        ] {
            assert_eq!(directions[&tag.parse().unwrap()].as_str(), expected);
        }
        let fallbacks = preload_fallbacks(&locales);
        assert_eq!(fallbacks[&"zh-Hant".parse().unwrap()], None);
        assert_eq!(
            fallbacks[&"es-AR".parse().unwrap()],
            Some("es-419".parse().unwrap())
        );
    }
}
