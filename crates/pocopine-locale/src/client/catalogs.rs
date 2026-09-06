use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use crate::{
    Catalog, CatalogAudience, CatalogIdentity, Locale, Locales, MessageId, PreparedCatalog,
    TranslationError, Value,
};

/// Validated catalogs loaded by the application boot/locale-switch pipeline.
/// Language selection belongs to the caller's reactive state; this cache has
/// no current locale. A failed install leaves every previous catalog intact.
#[derive(Clone)]
pub struct ClientCatalogs(Rc<CatalogCache>);

struct CatalogCache {
    locales: Locales,
    build_id: String,
    message_count: usize,
    catalogs: RefCell<BTreeMap<Locale, Rc<PreparedCatalog>>>,
}
impl ClientCatalogs {
    pub fn new(
        locales: Locales,
        build_id: &str,
        message_count: usize,
    ) -> Result<Self, TranslationError> {
        CatalogIdentity::new(
            build_id.into(),
            locales.default_locale().clone(),
            CatalogAudience::Browser,
            message_count,
        )?;
        Ok(Self(Rc::new(CatalogCache {
            locales,
            build_id: build_id.into(),
            message_count,
            catalogs: RefCell::new(BTreeMap::new()),
        })))
    }
    pub fn locales(&self) -> &Locales {
        &self.0.locales
    }
    /// Clones share one cache, so generated Rust calls and reactive template
    /// bindings observe the same validated installation.
    pub fn install(&self, locale: Locale, bytes: &[u8]) -> Result<(), TranslationError> {
        if !self
            .0
            .locales
            .supported()
            .any(|configured| configured == &locale)
        {
            return Err(TranslationError::Initialization(format!(
                "unexpected browser catalog locale {locale}"
            )));
        }
        let identity = CatalogIdentity::new(
            self.0.build_id.clone(),
            locale.clone(),
            CatalogAudience::Browser,
            self.0.message_count,
        )?;
        let prepared = PreparedCatalog::new(Catalog::load(bytes, &identity)?)?;
        self.0
            .catalogs
            .borrow_mut()
            .insert(locale, Rc::new(prepared));
        Ok(())
    }
    pub fn catalog(&self, locale: &Locale) -> Result<Rc<PreparedCatalog>, TranslationError> {
        let effective = self.0.locales.resolve(locale);
        self.0
            .catalogs
            .borrow()
            .get(effective)
            .cloned()
            .ok_or_else(|| TranslationError::CatalogNotLoaded(effective.clone()))
    }
    pub fn format(
        &self,
        locale: &Locale,
        id: MessageId,
        args: &[(&str, Value<'_>)],
    ) -> Result<String, TranslationError> {
        self.catalog(locale)?.format(id, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CATALOG_FORMAT_VERSION, CatalogArtifact, CatalogEntry};
    #[wasm_bindgen_test::wasm_bindgen_test]
    fn failed_installs_preserve_ready_catalogs_and_unloaded_locales_stay_explicit() {
        let locales = Locales::new(
            "en".parse().unwrap(),
            ["en", "fr"].map(|l| l.parse().unwrap()),
        )
        .unwrap();
        let cache = ClientCatalogs::new(locales, &"a".repeat(64), 1).unwrap();
        let mut artifact = CatalogArtifact {
            format_version: CATALOG_FORMAT_VERSION,
            build_id: "a".repeat(64),
            locale: "en".parse().unwrap(),
            audience: CatalogAudience::Browser,
            messages: vec![Some(CatalogEntry {
                source_locale: "en".parse().unwrap(),
                message: "Ready".into(),
            })],
        };
        cache
            .install(
                artifact.locale.clone(),
                &serde_json::to_vec(&artifact).unwrap(),
            )
            .unwrap();
        let en = cache.catalog(&"en".parse().unwrap()).unwrap();
        artifact.build_id = "b".repeat(64);
        assert!(
            cache
                .install(
                    artifact.locale.clone(),
                    &serde_json::to_vec(&artifact).unwrap()
                )
                .is_err()
        );
        assert!(Rc::ptr_eq(
            &en,
            &cache.catalog(&"en".parse().unwrap()).unwrap()
        ));
        assert!(matches!(
            cache.catalog(&"fr".parse().unwrap()),
            Err(TranslationError::CatalogNotLoaded(_))
        ));
        assert_eq!(
            cache
                .format(&"en".parse().unwrap(), MessageId(0), &[])
                .unwrap(),
            "Ready"
        );
    }
}
