#![cfg(not(target_arch = "wasm32"))]

use icu_plurals::provider::rules::runtime::{ast::Rule, test_rule};
use pocopine_locale::{CardinalRule, Locale, PluralArg};

/// Compare every vendored language with an independently implemented CLDR
/// engine. Feed ICU4X the source rules directly: its default baked data omits
/// some CLDR languages (such as Aragonese) and silently falls back to `other`.
/// Include boundaries and fractions; large magnitudes exceed f64 precision.
#[test]
fn generated_rules_match_icu4x() {
    let source: serde_json::Value =
        serde_json::from_str(include_str!("../data/cldr/plurals.json")).unwrap();
    let locales = source["supplemental"]["plurals-type-cardinal"]
        .as_object()
        .unwrap();
    let mut cases: Vec<String> = (0..=250).map(|i| i.to_string()).collect();
    for i in [0, 1, 2, 3, 4, 5, 10, 11, 12, 21, 99, 100, 1000, 1_000_000] {
        for fraction in ["0", "00", "1", "01", "10", "11", "20", "99", "001"] {
            cases.push(format!("{i}.{fraction}"));
        }
    }
    cases.extend(
        [
            "100000",
            "1000000",
            "2000000",
            "1000001",
            "9007199254740991",
            "9007199254740993",
            "18446744073709551615",
        ]
        .map(str::to_owned),
    );
    let mut checked = 0;
    for tag in locales.keys() {
        let locale: Locale = tag.parse().unwrap();
        let ours = CardinalRule::for_locale(&locale);
        let oracle: Vec<(&str, Rule<'_>)> = ["zero", "one", "two", "few", "many"]
            .into_iter()
            .filter_map(|category| {
                locales[tag]
                    .get(format!("pluralRule-count-{category}"))
                    .map(|rule| (category, rule.as_str().unwrap().parse().unwrap()))
            })
            .collect();
        for case in &cases {
            let arg: PluralArg = case.parse().unwrap();
            let operands: icu_plurals::PluralOperands = case.parse().unwrap();
            let actual = ours.category(arg).as_str();
            let expected = oracle
                .iter()
                .find(|(_, rule)| test_rule(rule, &operands))
                .map_or("other", |(category, _)| *category);
            assert_eq!(actual, expected, "locale={tag}, input={case}");
            checked += 1;
        }
    }
    eprintln!(
        "{checked} plural cases matched ICU4X across {} locales",
        locales.len()
    );
}
