//! Run with POCOPINE_LOCALE_DATA_DIR pointing at a CLI-generated en/fr/ar pack.
#![cfg(pocopine_locale_data)]

use pocopine_locale::{DateTimeArg, Message, MessageFormatter, TimeZone, Value};

fn format(tag: &str, text: &str, args: &[(&str, Value<'_>)]) -> String {
    let locale = tag.parse().unwrap();
    let message = Message::parse(text).unwrap();
    let parts = message.parts(&locale, args).unwrap();
    MessageFormatter::new(locale)
        .unwrap()
        .format(&parts)
        .unwrap()
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn configured_rules_preserve_exact_decimal_branches() {
    let text = "{n, plural, zero {zero} one {one} two {two} few {few} many {many} other {other}}";
    for (tag, value, expected) in [
        ("en", "1", "one"),
        ("en", "1.00", "other"),
        ("fr", "1.00", "one"),
        ("ar", "0", "zero"),
        ("ar", "2", "two"),
        ("ar", "3", "few"),
        ("ar", "11", "many"),
        ("ar", "3.1", "other"),
    ] {
        assert_eq!(
            format(tag, text, &[("n", Value::Number(value.parse().unwrap()))]),
            expected
        );
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn configured_formatter_loads_numbers_dates_and_recipient_timezones() {
    for (tag, expected) in [("en", "1,234.56"), ("fr", "1\u{202f}234,56")] {
        assert_eq!(
            format(
                tag,
                "{n, number}",
                &[("n", Value::Number("1234.56".parse().unwrap()))]
            ),
            expected
        );
    }
    let date = DateTimeArg::new(0, TimeZone::utc()).unwrap();
    assert_eq!(
        format("en", "{at, date, long}", &[("at", Value::DateTime(&date))]),
        "January 1, 1970"
    );
    assert_eq!(
        format("fr", "{at, date, long}", &[("at", Value::DateTime(&date))]),
        "1 janvier 1970"
    );
    let recipient = DateTimeArg::new(0, TimeZone::parse("America/New_York").unwrap()).unwrap();
    assert_eq!(
        format(
            "en",
            "{at, date, long}",
            &[("at", Value::DateTime(&recipient))]
        ),
        "December 31, 1969"
    );
}
