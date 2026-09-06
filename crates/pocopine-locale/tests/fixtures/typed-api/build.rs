use pocopine_locale::server::{
    CatalogSource, MessageReference, ReferenceKind, Span, compile_catalogs, generate_rust,
};
use pocopine_locale::{CatalogAudience, Locales};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let locales = Locales::new(
        "en".parse().unwrap(),
        ["en", "fr"].map(|l| l.parse().unwrap()),
    )
    .unwrap();
    let sources = [
        ("en",r#"{"cart.items":"{count, plural, one {# item} other {# items}}","cart.title":"BROWSER_COPY_SENTINEL","auth.denied":"HOST_COPY_SENTINEL","common.unused":"UNUSED_COPY_SENTINEL","schedule.when":"{at, date, long}","type.match":"Keyword {self} {__locale}"}"#),
        ("fr",r#"{"cart.items":"{count, plural, one {# article} other {# articles}}","cart.title":"Titre","auth.denied":"Refusé","schedule.when":"{at, date, long}","type.match":"Mot clé {self} {__locale}"}"#),
    ].into_iter().enumerate().map(|(file,(locale,source))| {
        let mut data: serde_json::Value = serde_json::from_str(source).unwrap();
        for module in ["std", "String", "Result", "__runtime", "__state"] {
            data[format!("{module}.title")] = "Namespace".into();
        }
        for (key, en, fr) in [
            ("common.unauthorized", "Please sign in.", "Veuillez vous connecter."),
            ("common.forbidden", "Access denied.", "Accès interdit."),
            ("common.bad_request", "Invalid request.", "Requête invalide."),
            ("common.internal", "Something went wrong.", "Une erreur est survenue."),
        ] { data[key] = if locale == "en" { en } else { fr }.into(); }
        if cfg!(feature = "catalog-update") {
            data["a.first"] = "NEW_HOST_COPY_SENTINEL".into();
            data["auth.denied"] = if locale == "en" { "Updated denial" } else { "Accès refusé" }.into();
        }
        CatalogSource {locale:locale.parse().unwrap(),file:file as u32,source:data.to_string()}
    }).collect::<Vec<_>>();
    let mut references = [
        ("cart.items", "cart", CatalogAudience::Browser),
        ("cart.title", "cart", CatalogAudience::Browser),
        ("auth.denied", "auth", CatalogAudience::Host),
        ("schedule.when", "schedule", CatalogAudience::Host),
        ("type.match", "type", CatalogAudience::Browser),
        ("common.unauthorized", "common", CatalogAudience::Host),
        ("common.forbidden", "common", CatalogAudience::Host),
        ("common.bad_request", "common", CatalogAudience::Host),
        ("common.internal", "common", CatalogAudience::Host),
    ]
    .map(|(key, module, audience)| MessageReference {
        key: key.into(),
        module: module.into(),
        audience,
        kind: ReferenceKind::Rust,
        span: Span::UNKNOWN,
    })
    .to_vec();
    for module in ["std", "String", "Result", "__runtime", "__state"] {
        references.push(MessageReference {
            key: format!("{module}.title"),
            module: module.into(),
            audience: CatalogAudience::Browser,
            kind: ReferenceKind::Rust,
            span: Span::UNKNOWN,
        });
    }
    if cfg!(feature = "catalog-update") {
        references.push(MessageReference {
            key: "a.first".into(),
            module: "a".into(),
            audience: CatalogAudience::Host,
            kind: ReferenceKind::Rust,
            span: Span::UNKNOWN,
        });
    }
    let compiled = compile_catalogs(&locales, &sources, &references);
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(
        out.join("catalog-shape.json"),
        serde_json::json!({
            "build_id": compiled.build_id,
            "denied_id": compiled.messages["auth.denied"].id.0,
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        out.join("pocopine_locale.rs"),
        generate_rust(&compiled, &locales, "::pocopine_locale").unwrap(),
    )
    .unwrap();
    for catalog in compiled.catalogs {
        if catalog.artifact.audience == CatalogAudience::Browser {
            std::fs::write(
                out.join(format!("{}.json", catalog.artifact.locale)),
                catalog.bytes,
            )
            .unwrap();
        }
    }
}
