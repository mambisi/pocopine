#![cfg(all(target_arch = "wasm32", feature = "locale"))]

use pocopine_core::locale::{
    CATALOG_FORMAT_VERSION, CatalogArtifact, CatalogAudience, CatalogEntry, LocaleConfig,
    LocaleManifest, Locales, MessageId, RoutingMode,
    client::{ClientCatalogs, SwitchOutcome, boot},
};
use wasm_bindgen::prelude::*;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen(inline_js = r#"
export function shell(manifest) {
  window.__pocopineLocale = {
    manifest: JSON.parse(manifest), route: 'fr', cookie: 'en', accepted: 'en',
    responses: new Map(), ready: false, failed: 0, requests: 0,
    load(url) {
      this.requests++;
      const bytes = this.responses.get(url);
      return bytes ? Promise.resolve(bytes.slice().buffer) : Promise.reject(new Error('offline'));
    },
    markReady() { this.ready = true; },
    fail() { this.failed++; }
  };
}
export function response(url, bytes) { window.__pocopineLocale.responses.set(url, bytes.slice()); }
export function metric(name) { return Number(window.__pocopineLocale[name]); }
"#)]
extern "C" {
    fn shell(manifest: &str);
    fn response(url: &str, bytes: &[u8]);
    fn metric(name: &str) -> u32;
}

fn bytes(locale: &str, text: &str) -> Vec<u8> {
    serde_json::to_vec(&CatalogArtifact {
        format_version: CATALOG_FORMAT_VERSION,
        build_id: "a".repeat(64),
        locale: locale.parse().unwrap(),
        audience: CatalogAudience::Browser,
        messages: vec![Some(CatalogEntry {
            source_locale: locale.parse().unwrap(),
            message: text.into(),
        })],
    })
    .unwrap()
}

#[wasm_bindgen_test]
async fn boot_validates_before_activation_and_switch_retries_preserve_the_visible_language() {
    let locales = Locales::new(
        "en".parse().unwrap(),
        ["en", "fr"].map(|s| s.parse().unwrap()),
    )
    .unwrap();
    let cache = ClientCatalogs::new(locales, &"a".repeat(64), 1).unwrap();
    let mut manifest = LocaleManifest {
        format_version: CATALOG_FORMAT_VERSION,
        build_id: "b".repeat(64),
        message_count: 1,
        config: LocaleConfig {
            default: "en".parse().unwrap(),
            locales: vec!["en".parse().unwrap(), "fr".parse().unwrap()],
            routing: RoutingMode::PrefixExceptDefault,
            strict_parity: false,
        },
        catalogs: [
            ("en".parse().unwrap(), "/pkg/locales/en.json".into()),
            ("fr".parse().unwrap(), "/pkg/locales/fr.json".into()),
        ]
        .into(),
        directions: ["en", "fr"]
            .map(|l| {
                (
                    l.parse().unwrap(),
                    pocopine_core::locale::TextDirection::Ltr,
                )
            })
            .into(),
    };
    shell(&serde_json::to_string(&manifest).unwrap());
    assert!(boot(cache.clone()).await.is_err());
    assert_eq!(metric("requests"), 0);
    assert_eq!(metric("failed"), 1);
    assert_eq!(metric("ready"), 0);
    manifest.build_id = "a".repeat(64);
    shell(&serde_json::to_string(&manifest).unwrap());
    assert!(boot(cache.clone()).await.is_err());
    assert_eq!(metric("failed"), 1);
    response("/pkg/locales/fr.json", &bytes("en", "Wrong catalog"));
    assert!(boot(cache.clone()).await.is_err());
    assert_eq!(metric("ready"), 0);
    response("/pkg/locales/fr.json", &bytes("fr", "Bonjour"));
    let controller = boot(cache).await.unwrap();
    assert_eq!(metric("ready"), 1);
    assert_eq!(controller.snapshot().as_str(), "fr");
    let root = web_sys::window()
        .unwrap()
        .document()
        .unwrap()
        .document_element()
        .unwrap();
    assert_eq!(root.get_attribute("lang").as_deref(), Some("fr"));
    assert_eq!(root.get_attribute("dir").as_deref(), Some("ltr"));
    assert_eq!(controller.format(MessageId(0), &[]).unwrap(), "Bonjour");
    assert!(controller.set_locale("en".parse().unwrap()).await.is_err());
    assert_eq!(controller.snapshot().as_str(), "fr");
    response("/pkg/locales/en.json", &bytes("en", "Hello"));
    assert_eq!(
        controller.set_locale("en".parse().unwrap()).await.unwrap(),
        SwitchOutcome::Committed
    );
    assert_eq!(controller.format(MessageId(0), &[]).unwrap(), "Hello");
    assert_eq!(root.get_attribute("lang").as_deref(), Some("en"));
    let requests = metric("requests");
    controller.set_locale("fr".parse().unwrap()).await.unwrap();
    assert_eq!(metric("requests"), requests, "cached switch must not fetch");
}
