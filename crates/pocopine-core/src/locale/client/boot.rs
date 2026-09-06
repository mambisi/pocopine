use pocopine_locale::{LocaleManifest, LocalePreferences, TranslationError};
use serde::Deserialize;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use super::{ClientCatalogs, LocaleController};

#[wasm_bindgen(inline_js = r#"
export function locale_boot_snapshot() {
  const boot = window.__pocopineLocale;
  if (!boot) throw new Error('locale HTML shell is missing; rebuild with the Pocopine CLI');
  return JSON.stringify({manifest: boot.manifest, route: boot.route,
    cookie: boot.cookie, accepted: boot.accepted});
}
export function locale_catalog_load(url) {
  const boot = window.__pocopineLocale;
  if (!boot) throw new Error('locale HTML shell is missing');
  return boot.load(url);
}
export function locale_boot_ready() { window.__pocopineLocale.markReady(); }
export function locale_boot_failed() {
  if (window.__pocopineLocale) window.__pocopineLocale.fail();
}
"#)]
extern "C" {
    #[wasm_bindgen(catch)]
    fn locale_boot_snapshot() -> Result<String, JsValue>;
    #[wasm_bindgen(catch)]
    fn locale_catalog_load(url: &str) -> Result<js_sys::Promise, JsValue>;
    fn locale_boot_ready();
    fn locale_boot_failed();
}

#[derive(Deserialize)]
struct BootSnapshot {
    manifest: LocaleManifest,
    route: Option<String>,
    cookie: Option<String>,
    accepted: String,
}

/// Finish the CLI shell's parallel catalog load, then activate translations.
/// Call `t::initialize(t::locales())?` first, pass `t::catalogs()?`, and await
/// this function before mounting the application. Failure leaves the shell's
/// visible reload action in place and does not publish a partial locale.
pub async fn boot(catalogs: ClientCatalogs) -> Result<LocaleController, TranslationError> {
    let result = initialize(catalogs).await;
    if result.is_err() {
        locale_boot_failed();
    }
    result
}

async fn initialize(catalogs: ClientCatalogs) -> Result<LocaleController, TranslationError> {
    let snapshot = locale_boot_snapshot().map_err(js_error)?;
    let snapshot: BootSnapshot = serde_json::from_str(&snapshot)
        .map_err(|e| TranslationError::Initialization(e.to_string()))?;
    snapshot.manifest.validate(
        catalogs.locales(),
        catalogs.build_id(),
        catalogs.message_count(),
    )?;
    let selected = catalogs
        .locales()
        .negotiate(LocalePreferences {
            route: snapshot.route.as_deref(),
            cookie: snapshot.cookie.as_deref(),
            accepted: &snapshot.accepted,
            ..Default::default()
        })
        .locale;
    let url = &snapshot.manifest.catalogs[&selected];
    let bytes = load_catalog(url).await?;
    catalogs.install(selected.clone(), &bytes)?;
    let controller = LocaleController::with_delivery(catalogs, selected, Some(snapshot.manifest))?;
    controller.activate()?;
    controller.update_document(&controller.snapshot())?;
    locale_boot_ready();
    Ok(controller)
}

pub(super) async fn load_catalog(url: &str) -> Result<Vec<u8>, TranslationError> {
    let promise = locale_catalog_load(url).map_err(js_error)?;
    let bytes = JsFuture::from(promise).await.map_err(js_error)?;
    Ok(js_sys::Uint8Array::new(&bytes).to_vec())
}

fn js_error(value: JsValue) -> TranslationError {
    let message = js_sys::Reflect::get(&value, &"message".into())
        .ok()
        .and_then(|value| value.as_string())
        .or_else(|| value.as_string())
        .unwrap_or_else(|| "catalog request failed".into());
    TranslationError::Initialization(message)
}

pub(super) fn update_document(
    locale: &pocopine_locale::Locale,
    direction: pocopine_locale::TextDirection,
) -> Result<(), TranslationError> {
    let root = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
        .ok_or_else(|| TranslationError::Initialization("document root is missing".into()))?;
    root.set_attribute("lang", locale.as_str())
        .map_err(js_error)?;
    root.set_attribute("dir", direction.as_str())
        .map_err(js_error)
}
