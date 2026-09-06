use pocopine_locale::{
    DateTimeArg, DateTimeStyle, Message, MessageFormatter, MessagePart, RenderError, RenderedPart,
    StyleLength, TimeZone, Value,
};

fn format(locale: &str, source: &str, args: &[(&str, Value<'_>)]) -> String {
    let locale = locale.parse().unwrap();
    let message = Message::parse(source).unwrap();
    let parts = message.parts(&locale, args).unwrap();
    MessageFormatter::new(locale)
        .unwrap()
        .format(&parts)
        .unwrap()
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn exact_number_precision_and_localized_percent_placement() {
    for (locale, value, pattern, expected) in [
        ("en", "1234567.8901", "{n, number}", "1,234,567.89"),
        (
            "fr",
            "1234567.8901",
            "{n, number}",
            "1\u{202f}234\u{202f}567,89",
        ),
        (
            "en",
            "18446744073709551615",
            "{n, number}",
            "18,446,744,073,709,551,615",
        ),
        ("en", "1.2345", "{n, number}", "1.235"),
        ("en", "-1.2345", "{n, number}", "-1.235"),
        ("en", "-0.0001", "{n, number}", "-0"),
        ("en", "1.005", "{n, number, percent}", "101%"),
        ("fr", "0.5", "{n, number, percent}", "50\u{a0}%"),
        ("tr", "0.5", "{n, number, percent}", "%50"),
    ] {
        let args = [("n", Value::Number(value.parse().unwrap()))];
        assert_eq!(
            format(locale, pattern, &args),
            expected,
            "{locale} {value} {pattern}"
        );
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn visible_fraction_still_decides_plural_before_display_rounding() {
    let pattern = "{n, plural, one {one: #} other {other: #}}";
    assert_eq!(
        format("en", pattern, &[("n", Value::Number("1".parse().unwrap()))]),
        "one: 1"
    );
    assert_eq!(
        format(
            "en",
            pattern,
            &[("n", Value::Number("1.00".parse().unwrap()))]
        ),
        "other: 1"
    );
    assert_eq!(
        format(
            "es",
            pattern,
            &[("n", Value::Number("1.00".parse().unwrap()))]
        ),
        "one: 1"
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn recipient_timezones_control_day_rollover_and_dst() {
    let source = "{at, date, long} {at, time, short}";
    // 2024-02-29T23:30:00Z: Dubai is already March 1.
    let utc = DateTimeArg::new(1_709_249_400_000, TimeZone::utc()).unwrap();
    let dubai =
        DateTimeArg::new(utc.unix_millis(), TimeZone::parse("Asia/Dubai").unwrap()).unwrap();
    assert_eq!(
        format("en-GB", source, &[("at", Value::DateTime(&utc))]),
        "29 February 2024 23:30"
    );
    assert_eq!(
        format("en-GB", source, &[("at", Value::DateTime(&dubai))]),
        "1 March 2024 03:30"
    );
    // US spring-forward: an hour later, wall time advances two hours.
    let zone = TimeZone::parse("America/New_York").unwrap();
    let before = DateTimeArg::new(1_710_052_200_000, zone.clone()).unwrap();
    let after = DateTimeArg::new(before.unix_millis() + 3_600_000, zone).unwrap();
    assert_eq!(
        format(
            "en-GB",
            "{at, time, short}",
            &[("at", Value::DateTime(&before))]
        ),
        "01:30"
    );
    assert_eq!(
        format(
            "en-GB",
            "{at, time, short}",
            &[("at", Value::DateTime(&after))]
        ),
        "03:30"
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn invalid_recipient_inputs_fail_at_construction_and_deserialization() {
    for name in ["", "Not/A_Zone", "+04:00", "../UTC"] {
        assert!(TimeZone::parse(name).is_err(), "{name}");
    }
    assert!(DateTimeArg::new(i64::MAX, TimeZone::utc()).is_err());
    assert!(
        serde_json::from_str::<DateTimeArg>(
            r#"{"unix_millis":9223372036854775807,"time_zone":"UTC"}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<DateTimeArg>(r#"{"unix_millis":0,"time_zone":"Not/A_Zone"}"#)
            .is_err()
    );
    let at = DateTimeArg::new(0, TimeZone::parse("Europe/Paris").unwrap()).unwrap();
    assert_eq!(
        serde_json::from_str::<DateTimeArg>(&serde_json::to_string(&at).unwrap()).unwrap(),
        at
    );
    let formatter = MessageFormatter::new("en".parse().unwrap()).unwrap();
    assert!(
        formatter
            .format(&[MessagePart::DateTime {
                value: &at,
                style: DateTimeStyle::Time(StyleLength::Long),
            }])
            .is_err()
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn rich_messages_keep_element_markers_and_plain_text_calls_refuse_them() {
    let message = Message::parse("<1>{name}</1> <0>details</0>").unwrap();
    let locale = "en".parse().unwrap();
    let args = [("name", Value::Text("<script>user text</script>"))];
    let parts = message.parts(&locale, &args).unwrap();
    let formatter = MessageFormatter::new(locale).unwrap();
    assert_eq!(
        formatter.format(&parts),
        Err(RenderError::ElementPlaceholdersRequireTemplate)
    );
    assert_eq!(
        formatter.render(&parts).unwrap(),
        vec![
            RenderedPart::OpenElement(1),
            RenderedPart::Text("<script>user text</script>".into()),
            RenderedPart::CloseElement(1),
            RenderedPart::Text(" ".into()),
            RenderedPart::OpenElement(0),
            RenderedPart::Text("details".into()),
            RenderedPart::CloseElement(0),
        ]
    );
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn host_formatters_are_send_sync_and_keep_concurrent_locales_separate() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<MessageFormatter>();
    let en = std::sync::Arc::new(MessageFormatter::new("en".parse().unwrap()).unwrap());
    let fr = std::sync::Arc::new(MessageFormatter::new("fr".parse().unwrap()).unwrap());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let en = en.clone();
            let fr = fr.clone();
            scope.spawn(move || {
                let message = Message::parse("{n, number}").unwrap();
                let args = [("n", Value::Number("1234.5".parse().unwrap()))];
                for _ in 0..20 {
                    assert_eq!(
                        en.format(&message.parts(en.locale(), &args).unwrap())
                            .unwrap(),
                        "1,234.5"
                    );
                    assert_eq!(
                        fr.format(&message.parts(fr.locale(), &args).unwrap())
                            .unwrap(),
                        "1\u{202f}234,5"
                    );
                }
            });
        }
    });
}
