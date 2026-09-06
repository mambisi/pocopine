use std::collections::{BTreeMap, BTreeSet};

use super::{Diagnostic, Severity, SourceMessages, Span, parse_messages};
use crate::{
    ArgumentKind, CATALOG_FORMAT_VERSION, CLDR_VERSION, CatalogArtifact, CatalogAudience,
    CatalogEntry, Locale, Locales, Message, MessageId,
};

#[derive(Clone, Debug)]
pub struct CatalogSource {
    pub locale: Locale,
    pub file: u32,
    pub source: String,
}

#[derive(Clone, Debug)]
pub enum ReferenceKind {
    /// Named pp-t arguments and the number of template-owned child elements.
    Text {
        arguments: Vec<String>,
        elements: usize,
    },
    Attribute,
    /// The generated Rust signature enforces arity/types at its call site.
    Rust,
    /// A Rust use/re-export. A catalog namespace imports no message; importing
    /// a function retains it even when calls use a re-exported path.
    RustImport,
}

#[derive(Clone, Debug)]
pub struct MessageReference {
    pub key: String,
    pub module: String,
    pub kind: ReferenceKind,
    pub audience: CatalogAudience,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct MessageSignature {
    pub id: MessageId,
    pub arguments: BTreeMap<String, ArgumentKind>,
    pub elements: BTreeSet<u16>,
    pub browser: bool,
}

#[derive(Clone, Debug)]
pub struct CompiledCatalog {
    pub artifact: CatalogArtifact,
    pub bytes: Vec<u8>,
    /// Content-addressed basename. The caller places Host artifacts outside
    /// the web asset directory and Browser artifacts inside it.
    pub filename: String,
}

#[derive(Debug, Default)]
pub struct Compilation {
    pub build_id: Option<String>,
    pub messages: BTreeMap<String, MessageSignature>,
    pub catalogs: Vec<CompiledCatalog>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Compilation {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

/// Validate the complete source catalogs and references, then assign stable
/// sorted IDs and produce both browser and host artifacts. No partial output
/// is returned on errors. IO and source discovery belong to the caller.
pub fn compile_catalogs(
    locales: &Locales,
    sources: &[CatalogSource],
    references: &[MessageReference],
) -> Compilation {
    let mut out = Compilation::default();
    compile(locales, sources, references, &mut out);
    out.diagnostics.sort_by(|a, b| {
        (a.span.file, a.span.start, &a.message).cmp(&(b.span.file, b.span.start, &b.message))
    });
    if out.has_errors() {
        out.catalogs.clear();
        out.messages.clear();
        out.build_id = None;
    }
    out
}

fn compile(
    locales: &Locales,
    sources: &[CatalogSource],
    references: &[MessageReference],
    out: &mut Compilation,
) {
    let supported: BTreeSet<_> = locales.supported().cloned().collect();
    let mut files: BTreeMap<Locale, SourceMessages> = BTreeMap::new();
    let mut parsed: BTreeMap<(Locale, String), Message> = BTreeMap::new();
    for source in sources {
        if !supported.contains(&source.locale) {
            out.diagnostics.push(
                Diagnostic::error(format!("locale {} is not configured", source.locale)).at(Span {
                    file: source.file,
                    start: 0,
                    end: 0,
                }),
            );
            continue;
        }
        let messages = match parse_messages(&source.source, source.file) {
            Ok(messages) => messages,
            Err(error) => {
                out.diagnostics.push(error);
                continue;
            }
        };
        for (key, value) in &messages {
            if !valid_key(key) {
                out.diagnostics.push(
                    Diagnostic::error(format!("invalid dotted message key {key:?}")).at(value.span),
                );
            }
            let mut prefix = key.as_str();
            while let Some((parent, _)) = prefix.rsplit_once('.') {
                if messages.contains_key(parent) {
                    out.diagnostics.push(
                        Diagnostic::error(format!(
                            "message key {parent:?} is both a leaf and a namespace"
                        ))
                        .at(value.span),
                    );
                }
                prefix = parent;
            }
            match Message::parse(&value.text) {
                Ok(message) => {
                    parsed.insert((source.locale.clone(), key.clone()), message);
                }
                Err(error) => out
                    .diagnostics
                    .push(Diagnostic::error(format!("{key}: {error}")).at(value.span)),
            }
        }
        if files.insert(source.locale.clone(), messages).is_some() {
            out.diagnostics.push(Diagnostic::error(format!(
                "duplicate catalog for locale {}",
                source.locale
            )));
        }
    }
    for locale in &supported {
        if !files.contains_key(locale) {
            out.diagnostics.push(Diagnostic::error(format!(
                "missing source catalog for locale {locale}"
            )));
        }
    }
    let Some(default) = files.get(locales.default_locale()) else {
        return;
    };
    let mut reached: BTreeMap<String, bool> = BTreeMap::new();
    for reference in references {
        if matches!(reference.kind, ReferenceKind::RustImport)
            && (reference.key.is_empty()
                || (!default.contains_key(&reference.key)
                    && default
                        .keys()
                        .any(|key| key.starts_with(&format!("{}.", reference.key)))))
        {
            continue;
        }
        if !reference.key.starts_with("common.")
            && !reference.key.starts_with(&format!("{}.", reference.module))
        {
            out.diagnostics.push(
                Diagnostic::error(format!(
                    "{} belongs to another module; use {}.* or common.*",
                    reference.key, reference.module
                ))
                .at(reference.span),
            );
        }
        if !default.contains_key(&reference.key) {
            out.diagnostics.push(
                Diagnostic::error(format!(
                    "message {} is missing from default locale {}",
                    reference.key,
                    locales.default_locale()
                ))
                .at(reference.span),
            );
            continue;
        }
        let Some(message) = parsed.get(&(locales.default_locale().clone(), reference.key.clone()))
        else {
            continue;
        };
        match &reference.kind {
            ReferenceKind::Text {
                arguments,
                elements,
            } => {
                let supplied: BTreeSet<_> = arguments.iter().cloned().collect();
                let expected: BTreeSet<_> = message.arguments().keys().cloned().collect();
                if supplied.len() != arguments.len() || supplied != expected {
                    out.diagnostics.push(
                        Diagnostic::error(format!(
                            "{} requires arguments {expected:?}; received {arguments:?}",
                            reference.key
                        ))
                        .at(reference.span),
                    );
                }
                let expected_elements: BTreeSet<_> = (0..*elements)
                    .filter_map(|i| u16::try_from(i).ok())
                    .collect();
                if *elements > u16::MAX as usize || message.elements() != &expected_elements {
                    out.diagnostics.push(
                        Diagnostic::error(format!(
                            "{} element placeholders do not match its {elements} template children",
                            reference.key
                        ))
                        .at(reference.span),
                    );
                }
            }
            ReferenceKind::Attribute
                if !message.arguments().is_empty() || !message.elements().is_empty() =>
            {
                out.diagnostics.push(Diagnostic::error(format!("$t.{} requires a text-only message without arguments; use a computed Rust translation for argument-taking attributes", reference.key)).at(reference.span));
            }
            ReferenceKind::Rust | ReferenceKind::RustImport if !message.elements().is_empty() => {
                out.diagnostics.push(
                    Diagnostic::error(format!(
                        "{} has element placeholders and requires pp-t template rendering",
                        reference.key
                    ))
                    .at(reference.span),
                );
            }
            _ => {}
        }
        *reached.entry(reference.key.clone()).or_default() |=
            reference.audience == CatalogAudience::Browser;
    }
    for (locale, messages) in &files {
        for (key, value) in messages {
            if !reached.contains_key(key) {
                out.diagnostics.push(
                    Diagnostic::warning(format!(
                        "orphaned message {key} in {locale}; it will not be shipped"
                    ))
                    .at(value.span),
                );
            }
            if locale == locales.default_locale() {
                continue;
            }
            if let (Some(base), Some(translated)) = (
                parsed.get(&(locales.default_locale().clone(), key.clone())),
                parsed.get(&(locale.clone(), key.clone())),
            ) {
                if base.arguments() != translated.arguments()
                    || base.elements() != translated.elements()
                {
                    out.diagnostics.push(Diagnostic::error(format!("{key} in {locale} changes the default-locale argument types or element placeholders")).at(value.span));
                }
            } else if !default.contains_key(key) {
                out.diagnostics.push(
                    Diagnostic::error(format!("{key} in {locale} has no default-locale contract"))
                        .at(value.span),
                );
            }
        }
    }
    if out.has_errors() {
        return;
    }
    for (index, (key, browser)) in reached.iter().enumerate() {
        let message = &parsed[&(locales.default_locale().clone(), key.clone())];
        out.messages.insert(
            key.clone(),
            MessageSignature {
                id: MessageId(index as u32),
                arguments: message.arguments().clone(),
                elements: message.elements().clone(),
                browser: *browser,
            },
        );
    }
    if out.messages.len() > 100_000 {
        out.diagnostics.push(Diagnostic::error(
            "project exceeds 100000 referenced messages",
        ));
        return;
    }
    // Hash semantic inputs only. Paths, source ordering, and JSON whitespace
    // do not affect build identity; messages, target reachability, and locale
    // ordering do. No raw digest implementations in consumers.
    let content: Vec<_> = files
        .iter()
        .map(|(locale, messages)| {
            (
                locale,
                messages
                    .iter()
                    .filter(|(key, _)| reached.contains_key(*key))
                    .map(|(key, value)| (key, &value.text))
                    .collect::<BTreeMap<_, _>>(),
            )
        })
        .collect();
    let identity = serde_json::to_vec(&(
        CATALOG_FORMAT_VERSION,
        CLDR_VERSION,
        locales.default_locale(),
        &supported,
        &reached,
        content,
    ))
    .expect("serializable compiler inputs");
    let build_id = pocopine_crypto::sha256_hex(&identity);
    for locale in &supported {
        let chain = locales.fallback_chain(locale);
        let mut entries = Vec::with_capacity(reached.len());
        for key in reached.keys() {
            let (origin, value) = chain
                .iter()
                .find_map(|origin| files[*origin].get(key).map(|value| (*origin, value)))
                .expect("validated default locale contract");
            if origin != locale {
                out.diagnostics.push(Diagnostic::warning(format!(
                    "missing translation {key} in {locale}; falling back to {origin}"
                )));
            }
            entries.push(Some(CatalogEntry {
                source_locale: origin.clone(),
                message: value.text.clone(),
            }));
        }
        for audience in [CatalogAudience::Browser, CatalogAudience::Host] {
            let mut messages = entries.clone();
            if audience == CatalogAudience::Browser {
                for (entry, browser) in messages.iter_mut().zip(reached.values()) {
                    if !browser {
                        *entry = None;
                    }
                }
            }
            let artifact = CatalogArtifact {
                format_version: CATALOG_FORMAT_VERSION,
                build_id: build_id.clone(),
                locale: locale.clone(),
                audience,
                messages,
            };
            let bytes = serde_json::to_vec(&artifact).expect("serializable catalog artifact");
            if bytes.len() > crate::catalog::MAX_CATALOG_BYTES {
                out.diagnostics.push(Diagnostic::error(format!(
                    "compiled {locale} {audience:?} catalog exceeds 16 MiB"
                )));
                continue;
            }
            let hash = pocopine_crypto::sha256_hex(&bytes);
            let target = match audience {
                CatalogAudience::Browser => "browser",
                CatalogAudience::Host => "host",
            };
            out.catalogs.push(CompiledCatalog {
                filename: format!("{locale}.{target}.{hash}.json"),
                artifact,
                bytes,
            });
        }
    }
    out.build_id = Some(build_id);
}

fn valid_key(key: &str) -> bool {
    key.contains('.')
        && key.split('.').all(|part| {
            !["_", "self", "Self", "super", "crate"].contains(&part)
                && part.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Catalog, CatalogIdentity, MessagePart, Value};

    fn setup() -> (Locales, Vec<CatalogSource>, Vec<MessageReference>) {
        let locales = Locales::new(
            "en".parse().unwrap(),
            ["en", "fr", "fr-CA"].map(|s| s.parse().unwrap()),
        )
        .unwrap();
        let sources = [("en",r#"{"cart.items":"{n, plural, one {one} other {many}}","auth.failed":"Sign-in failed","common.unused":"UNUSED_SENTINEL"}"#),("fr",r#"{"cart.items":"{n, plural, one {un} other {plusieurs}}"}"#),("fr-CA","{}")].into_iter().enumerate().map(|(file,(locale,source))| CatalogSource { locale: locale.parse().unwrap(), file: file as u32, source: source.into() }).collect();
        let references = vec![
            MessageReference {
                key: "cart.items".into(),
                module: "cart".into(),
                kind: ReferenceKind::Text {
                    arguments: vec!["n".into()],
                    elements: 0,
                },
                audience: CatalogAudience::Browser,
                span: Span::UNKNOWN,
            },
            MessageReference {
                key: "auth.failed".into(),
                module: "auth".into(),
                kind: ReferenceKind::Rust,
                audience: CatalogAudience::Host,
                span: Span::UNKNOWN,
            },
        ];
        (locales, sources, references)
    }

    #[test]
    fn deterministic_ids_fallback_and_target_pruning_are_loadable() {
        let (locales, mut sources, mut references) = setup();
        let a = compile_catalogs(&locales, &sources, &references);
        assert!(!a.has_errors(), "{:?}", a.diagnostics);
        assert_eq!(a.messages["auth.failed"].id, MessageId(0));
        assert_eq!(a.messages["cart.items"].id, MessageId(1));
        assert!(!a.messages.contains_key("common.unused"));
        for catalog in &a.catalogs {
            assert!(!String::from_utf8_lossy(&catalog.bytes).contains("UNUSED_SENTINEL"));
            let expected = CatalogIdentity::new(
                a.build_id.clone().unwrap(),
                catalog.artifact.locale.clone(),
                catalog.artifact.audience,
                2,
            )
            .unwrap();
            let loaded = Catalog::load(&catalog.bytes, &expected).unwrap();
            if catalog.artifact.audience == CatalogAudience::Browser {
                assert!(loaded.message(MessageId(0)).is_err());
            } else {
                assert!(loaded.message(MessageId(0)).is_ok());
            }
            if catalog.artifact.locale.as_str() == "fr-CA" {
                assert_eq!(
                    loaded
                        .message(MessageId(1))
                        .unwrap()
                        .source_locale()
                        .as_str(),
                    "fr"
                );
                let args = [("n", Value::Number(0u64.into()))];
                assert_eq!(
                    loaded.parts(MessageId(1), &args).unwrap(),
                    vec![MessagePart::Text("un".into())]
                );
            }
        }
        sources.reverse();
        references.reverse();
        let b = compile_catalogs(&locales, &sources, &references);
        assert_eq!(a.build_id, b.build_id);
        assert_eq!(
            a.catalogs.iter().map(|c| &c.bytes).collect::<Vec<_>>(),
            b.catalogs.iter().map(|c| &c.bytes).collect::<Vec<_>>()
        );
    }

    #[test]
    fn rejects_cross_module_missing_default_and_signature_drift_without_artifacts() {
        let (locales, mut sources, mut references) = setup();
        references[0].module = "orders".into();
        sources[1].source = r#"{"cart.items":"{wrong, number}"}"#.into();
        references[1].key = "auth.missing".into();
        let result = compile_catalogs(&locales, &sources, &references);
        assert!(result.has_errors());
        assert!(result.catalogs.is_empty());
        assert!(result.messages.is_empty());
        assert!(result.build_id.is_none());
        for expected in [
            "another module",
            "missing from default",
            "changes the default",
        ] {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains(expected)),
                "{expected}"
            );
        }
    }

    #[test]
    fn build_identity_tracks_shipped_copy_and_target_changes_but_not_orphan_copy() {
        let (locales, mut sources, mut references) = setup();
        let initial = compile_catalogs(&locales, &sources, &references)
            .build_id
            .unwrap();
        sources[0].source = sources[0]
            .source
            .replace("UNUSED_SENTINEL", "Unused copy edit");
        assert_eq!(
            compile_catalogs(&locales, &sources, &references)
                .build_id
                .as_deref(),
            Some(initial.as_str())
        );
        sources[0].source = sources[0]
            .source
            .replace("Sign-in failed", "Authentication failed");
        let copy_edit = compile_catalogs(&locales, &sources, &references)
            .build_id
            .unwrap();
        assert_ne!(copy_edit, initial);
        references[1].audience = CatalogAudience::Browser;
        assert_ne!(
            compile_catalogs(&locales, &sources, &references)
                .build_id
                .unwrap(),
            copy_edit
        );
    }

    #[test]
    fn refuses_attributes_with_arguments_and_leaf_namespace_collisions() {
        let (locales, mut sources, mut references) = setup();
        references[0].kind = ReferenceKind::Attribute;
        sources[0].source =
            r#"{"cart.items":"{n, number}","cart.items.count":"Count","auth.failed":"Failed"}"#
                .into();
        let result = compile_catalogs(&locales, &sources, &references);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("both a leaf"))
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("text-only"))
        );
        assert!(result.catalogs.is_empty());
    }

    #[test]
    fn rich_messages_require_template_owned_elements_instead_of_plain_rust_calls() {
        let (locales, mut sources, mut references) = setup();
        sources[0].source = r#"{"cart.items":"<0>Details</0>","auth.failed":"Failed"}"#.into();
        sources[1].source = "{}".into();
        references[0].kind = ReferenceKind::Rust;
        let result = compile_catalogs(&locales, &sources, &references);
        assert!(result.has_errors());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("requires pp-t"))
        );
        references[0].kind = ReferenceKind::Text {
            arguments: vec![],
            elements: 1,
        };
        let result = compile_catalogs(&locales, &sources, &references);
        assert!(!result.has_errors(), "{:?}", result.diagnostics);
    }
}
