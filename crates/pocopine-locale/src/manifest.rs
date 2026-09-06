use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{CATALOG_FORMAT_VERSION, Locale, LocaleConfig, Locales, TranslationError};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextDirection {
    Ltr,
    Rtl,
}

impl TextDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

/// Public delivery metadata. Catalog keys and host-only message contents never
/// enter this document; URLs name immutable browser catalog artifacts.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocaleManifest {
    pub format_version: u16,
    pub build_id: String,
    pub message_count: usize,
    pub config: LocaleConfig,
    pub catalogs: BTreeMap<Locale, String>,
    pub directions: BTreeMap<Locale, TextDirection>,
}

impl LocaleManifest {
    /// Validate delivery metadata against the API compiled into this bundle.
    pub fn validate(
        &self,
        locales: &Locales,
        build_id: &str,
        message_count: usize,
    ) -> Result<(), TranslationError> {
        let configured = self
            .config
            .validate()
            .map_err(|e| TranslationError::Initialization(e.to_string()))?;
        if self.format_version != CATALOG_FORMAT_VERSION
            || self.build_id != build_id
            || self.message_count != message_count
            || &configured != locales
            || !self.catalogs.keys().eq(locales.supported())
            || !self.directions.keys().eq(locales.supported())
        {
            return Err(TranslationError::Initialization(
                "locale manifest does not match this application build".into(),
            ));
        }
        for url in self.catalogs.values() {
            let filename = url.strip_prefix("/pkg/locales/").unwrap_or_default();
            if filename.is_empty()
                || !filename
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                || filename.contains("..")
            {
                return Err(TranslationError::Initialization(
                    "invalid catalog URL".into(),
                ));
            }
        }
        Ok(())
    }
}
