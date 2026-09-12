use super::*;
use pocopine_locale::{
    CatalogAudience,
    server::{CfgSet, DiscoveryOptions, SourceTarget, discover_project_with_options},
};

fn project() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("pocopine.toml"),
        "[locale]\ndefault='en'\nlocales=['en','fr']\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("lib.rs"),
        r#"
        #[component(template = poco! { <p pp-text="$t('common.welcome', name)"></p> })]
        struct App { name: String }
    "#,
    )
    .unwrap();
    temp
}

fn discover(project: &Path) -> ProjectDiscovery {
    discover_project_with_options(
        project,
        &[SourceTarget {
            path: project.join("lib.rs"),
            cfg: CfgSet::from_rustc("target_arch=\"wasm32\"\n").unwrap(),
            audience: CatalogAudience::Browser,
        }],
        DiscoveryOptions {
            allow_missing_catalogs: true,
        },
    )
}

#[test]
fn extraction_bootstraps_catalogs_preserves_existing_copy_and_has_a_fixed_point() {
    let temp = project();
    let discovery = discover(temp.path());
    assert!(!discovery.has_errors());
    extract(temp.path(), &discovery, &mut catalogs(&discovery).unwrap()).unwrap();
    let path = temp.path().join("locales/en.json");
    let en = std::fs::read(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<BTreeMap<String, String>>(&en).unwrap()["common.welcome"],
        ""
    );
    assert_eq!(
        std::fs::read(temp.path().join("locales/fr.json")).unwrap(),
        b"{}\n"
    );
    let notes = temp.path().join("locales/en.sources.json");
    let locations = std::fs::read(&notes).unwrap();
    assert!(
        String::from_utf8(locations.clone())
            .unwrap()
            .contains("lib.rs:")
    );
    let discovery = discover(temp.path());
    extract(temp.path(), &discovery, &mut catalogs(&discovery).unwrap()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), en);
    assert_eq!(std::fs::read(&notes).unwrap(), locations);
    std::fs::write(&path, "{\"common.welcome\":\"Hello {name}\"}\n").unwrap();
    let discovery = discover(temp.path());
    extract(temp.path(), &discovery, &mut catalogs(&discovery).unwrap()).unwrap();
    assert_eq!(
        parse_messages(&std::fs::read_to_string(&path).unwrap(), 0).unwrap()["common.welcome"].text,
        "Hello {name}"
    );
}

#[test]
fn invalid_json_updates_and_stale_xliff_do_not_overwrite_the_catalog() {
    let temp = project();
    std::fs::create_dir(temp.path().join("locales")).unwrap();
    std::fs::write(
        temp.path().join("locales/en.json"),
        r#"{"common.welcome":"Hello {name}"}"#,
    )
    .unwrap();
    let target = temp.path().join("locales/fr.json");
    let original = r#"{"common.welcome":"Bonjour {name}"}"#;
    std::fs::write(&target, original).unwrap();
    let discovery = discover(temp.path());
    let initial = catalogs(&discovery).unwrap();
    let locale = "fr".parse().unwrap();
    let invalid = parse_messages(r#"{"common.welcome":"Bonjour {nom}"}"#, 0).unwrap();
    assert!(
        merge(
            temp.path(),
            &discovery,
            &mut initial.clone(),
            &locale,
            invalid
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    let xml = export_xliff(
        &"en".parse().unwrap(),
        &locale,
        &initial[&"en".parse().unwrap()],
        &initial[&locale],
        &BTreeMap::new(),
    )
    .unwrap();
    let stale = import_xliff(&xml.replace("Hello {name}", "Old {name}")).unwrap();
    assert!(imported_updates(&discovery, &initial, stale).is_err());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), original);
    let changed = import_xliff(&xml.replace("Bonjour {name}", "Salut {name}")).unwrap();
    let (locale, updates) = imported_updates(&discovery, &initial, changed).unwrap();
    merge(
        temp.path(),
        &discovery,
        &mut initial.clone(),
        &locale,
        updates,
    )
    .unwrap();
    assert_eq!(
        parse_messages(&std::fs::read_to_string(&target).unwrap(), 0).unwrap()["common.welcome"]
            .text,
        "Salut {name}"
    );
}
