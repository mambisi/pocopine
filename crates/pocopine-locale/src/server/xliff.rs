//! XLIFF 2.0 exchange for complete MF1 messages. Each unit is one non-resegmented
//! message; positional element syntax remains escaped text, preserving the
//! catalog grammar across TMS tools. Inline XLIFF codes are rejected on import.

use std::collections::BTreeMap;

use quick_xml::{
    Writer,
    events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event},
};

use super::SourceMessages;
use crate::Locale;

const NS: &str = "urn:oasis:names:tc:xliff:document:2.0";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XliffUnit {
    pub source: String,
    pub target: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XliffDocument {
    pub source_locale: Locale,
    pub target_locale: Locale,
    pub units: BTreeMap<String, XliffUnit>,
}

pub fn export_xliff(
    source_locale: &Locale,
    target_locale: &Locale,
    source: &SourceMessages,
    target: &SourceMessages,
    locations: &BTreeMap<String, Vec<String>>,
) -> Result<String, String> {
    fn write(
        source_locale: &Locale,
        target_locale: &Locale,
        source: &SourceMessages,
        target: &SourceMessages,
        locations: &BTreeMap<String, Vec<String>>,
    ) -> std::io::Result<Vec<u8>> {
        let mut writer = Writer::new_with_indent(Vec::new(), b' ', 2);
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))?;
        writer
            .create_element("xliff")
            .with_attributes([
                ("xmlns", NS),
                ("version", "2.0"),
                ("srcLang", source_locale.as_str()),
                ("trgLang", target_locale.as_str()),
            ])
            .write_inner_content(|writer| {
                writer
                    .create_element("file")
                    .with_attributes([("id", "catalog"), ("canResegment", "no")])
                    .write_inner_content(|writer| {
                        for (index, (key, message)) in source.iter().enumerate() {
                            let id = format!("u{index}");
                            writer
                                .create_element("unit")
                                .with_attributes([("id", id.as_str()), ("name", key.as_str())])
                                .write_inner_content(|writer| {
                                    if let Some(locations) = locations.get(key) {
                                        writer.create_element("notes").write_inner_content(
                                            |writer| {
                                                for location in locations {
                                                    writer
                                                        .create_element("note")
                                                        .with_attribute(("category", "location"))
                                                        .write_text_content(xml_text(location))?;
                                                }
                                                Ok(())
                                            },
                                        )?;
                                    }
                                    writer.write_event(Event::Start(BytesStart::new("segment")))?;
                                    writer
                                        .create_element("source")
                                        .with_attribute(("xml:space", "preserve"))
                                        .write_text_content(xml_text(&message.text))?;
                                    if let Some(translated) = target.get(key) {
                                        writer
                                            .create_element("target")
                                            .with_attribute(("xml:space", "preserve"))
                                            .write_text_content(xml_text(&translated.text))?;
                                    }
                                    writer.write_event(Event::End(BytesEnd::new("segment")))?;
                                    Ok(())
                                })?;
                        }
                        Ok(())
                    })?;
                Ok(())
            })?;
        Ok(writer.into_inner())
    }
    let bytes = write(source_locale, target_locale, source, target, locations)
        .map_err(|e| e.to_string())?;
    let xml = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    // Refuse output that cannot round-trip (including XML 1.0 control chars).
    import_xliff(&xml)?;
    Ok(xml)
}

fn xml_text(text: &str) -> BytesText<'static> {
    // XML normalizes literal CR/CRLF. A character reference preserves the
    // catalog's exact source bytes for stale-source checks on reimport.
    BytesText::from_escaped(quick_xml::escape::escape(text).replace('\r', "&#13;"))
}

pub fn import_xliff(source: &str) -> Result<XliffDocument, String> {
    if source.len() > 16 * 1024 * 1024 {
        return Err("XLIFF input exceeds 16 MiB".into());
    }
    let doc = roxmltree::Document::parse_with_options(
        source,
        roxmltree::ParsingOptions {
            nodes_limit: 1_000_000,
            ..Default::default()
        },
    )
    .map_err(|e| format!("invalid XLIFF XML: {e}"))?;
    let root = doc.root_element();
    if !root.has_tag_name((NS, "xliff")) || root.attribute("version") != Some("2.0") {
        return Err("expected an XLIFF 2.0 document in the standard namespace".into());
    }
    let language = |name| {
        root.attribute(name)
            .ok_or_else(|| format!("XLIFF is missing {name}"))?
            .parse::<Locale>()
            .map_err(|e| e.to_string())
    };
    let mut output = XliffDocument {
        source_locale: language("srcLang")?,
        target_locale: language("trgLang")?,
        units: BTreeMap::new(),
    };
    let mut files = 0;
    for file in root.children().filter(|node| node.is_element()) {
        if !file.has_tag_name((NS, "file")) {
            return Err("XLIFF root may only contain files".into());
        }
        files += 1;
        if file.attribute("id").is_none() {
            return Err("XLIFF file needs an id".into());
        }
        for node in file.descendants().filter(|node| node.is_element()) {
            if node.tag_name().namespace() != Some(NS) {
                continue;
            }
            let parent = node
                .parent_element()
                .filter(|node| node.tag_name().namespace() == Some(NS))
                .map(|node| node.tag_name().name());
            let valid = matches!(
                (parent, node.tag_name().name()),
                (Some("xliff"), "file")
                    | (Some("file" | "group"), "group" | "unit" | "notes")
                    | (Some("unit"), "notes" | "segment")
                    | (Some("segment"), "source" | "target")
                    | (Some("notes"), "note")
            );
            if !valid {
                return Err(format!(
                    "unsupported XLIFF element {}; keep each MF1 message as plain text in one segment",
                    node.tag_name().name()
                ));
            }
        }
        for unit in file
            .descendants()
            .filter(|node| node.has_tag_name((NS, "unit")))
        {
            if unit.attribute("id").is_none() {
                return Err("XLIFF unit needs an id".into());
            }
            let key = unit
                .attribute("name")
                .or_else(|| unit.attribute("id"))
                .ok_or("XLIFF unit needs a message key in name or id")?;
            let segments = unit
                .children()
                .filter(|node| node.has_tag_name((NS, "segment")))
                .collect::<Vec<_>>();
            if segments.len() != 1 {
                return Err(format!("XLIFF unit {key} must contain exactly one segment"));
            }
            let segment = segments[0];
            let sources = segment
                .children()
                .filter(|node| node.has_tag_name((NS, "source")))
                .collect::<Vec<_>>();
            let targets = segment
                .children()
                .filter(|node| node.has_tag_name((NS, "target")))
                .collect::<Vec<_>>();
            if sources.len() != 1 || targets.len() > 1 {
                return Err(format!(
                    "XLIFF unit {key} needs one source and at most one target"
                ));
            }
            check_language(sources[0], &output.source_locale)?;
            if let Some(target) = targets.first() {
                check_language(*target, &output.target_locale)?;
            }
            let entry = XliffUnit {
                source: text(sources[0])?,
                target: targets.first().copied().map(text).transpose()?,
            };
            if output.units.insert(key.to_owned(), entry).is_some() {
                return Err(format!("duplicate XLIFF message key {key}"));
            }
        }
    }
    if files == 0 || output.units.is_empty() {
        return Err("XLIFF document contains no message units".into());
    }
    Ok(output)
}

fn text(node: roxmltree::Node<'_, '_>) -> Result<String, String> {
    if node.children().any(|child| child.is_element()) {
        return Err(
            "XLIFF source/target must contain plain MF1 text, without inline XML codes".into(),
        );
    }
    Ok(node
        .children()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text())
        .collect())
}

fn check_language(node: roxmltree::Node<'_, '_>, expected: &Locale) -> Result<(), String> {
    if let Some(tag) = node
        .ancestors()
        .find_map(|node| node.attribute(("http://www.w3.org/XML/1998/namespace", "lang")))
    {
        let actual = tag.parse::<Locale>().map_err(|e| e.to_string())?;
        if &actual != expected {
            return Err(format!(
                "XLIFF text language {actual} does not match document language {expected}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::parse_messages;

    #[test]
    fn exchange_preserves_whitespace_unicode_icu_and_escaped_element_syntax() {
        let source = parse_messages(r#"{"common.welcome":"  Hello {name}, <0>R&D</0>!\n","common.items":"{count, plural, one {# item} other {# items}}"}"#, 0).unwrap();
        let target = parse_messages(
            r#"{"common.welcome":"  Bonjour {name}, <0>R&D</0> !\n"}"#,
            1,
        )
        .unwrap();
        let xml = export_xliff(
            &"en".parse().unwrap(),
            &"fr".parse().unwrap(),
            &source,
            &target,
            &BTreeMap::new(),
        )
        .unwrap();
        assert!(xml.contains("&lt;0&gt;R&amp;D&lt;/0&gt;"));
        let parsed = import_xliff(&xml).unwrap();
        assert_eq!(
            parsed.units["common.welcome"].source,
            source["common.welcome"].text
        );
        assert_eq!(
            parsed.units["common.welcome"].target.as_deref(),
            Some(target["common.welcome"].text.as_str())
        );
        assert_eq!(parsed.units["common.items"].target, None);
    }

    #[test]
    fn malformed_ambiguous_or_resegmented_exchanges_do_not_partially_import() {
        let prefix =
            format!(r#"<xliff xmlns="{NS}" version="2.0" srcLang="en" trgLang="fr"><file id="f">"#);
        let unit =
            "<unit id=\"common.x\"><segment><source>x</source><target>y</target></segment></unit>";
        for body in [
            format!("{unit}{unit}"),
            unit.replace("<target>y</target>", "<target><ph id=\"1\"/></target>"),
            unit.replace("</unit>", "<segment><source>z</source></segment></unit>"),
            unit.replace("<source>x</source>", ""),
            format!("<notes><note>{unit}</note></notes>"),
            unit.replace("id=\"common.x\"", "name=\"common.x\""),
            unit.replace("<target>", "<target xml:lang=\"de\">"),
        ] {
            assert!(import_xliff(&format!("{prefix}{body}</file></xliff>")).is_err());
        }
        assert!(import_xliff("<!DOCTYPE x [<!ENTITY e 'x'>]><x>&e;</x>").is_err());
        assert!(
            import_xliff(&format!("{prefix}{unit}</file></xliff>").replace(NS, "invalid")).is_err()
        );
    }

    #[test]
    fn carriage_returns_round_trip_and_illegal_xml_characters_fail_export() {
        let mut source = parse_messages(r#"{"common.text":"first\r\nsecond\rthird"}"#, 0).unwrap();
        let locale = "en".parse().unwrap();
        let xml = export_xliff(
            &locale,
            &locale,
            &source,
            &SourceMessages::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(
            import_xliff(&xml).unwrap().units["common.text"].source,
            "first\r\nsecond\rthird"
        );
        source.get_mut("common.text").unwrap().text = "illegal\0".into();
        assert!(
            export_xliff(
                &locale,
                &locale,
                &source,
                &SourceMessages::new(),
                &BTreeMap::new()
            )
            .is_err()
        );
    }
}
