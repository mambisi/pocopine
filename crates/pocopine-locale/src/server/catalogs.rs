use std::{collections::BTreeMap, io::Read, path::Path};

use crate::{
    Catalog, CatalogAudience, CatalogIdentity, Locale, Locales, MessageId, PreparedCatalog,
    TranslationError, Value,
};

/// All host catalogs, validated and prepared before requests or jobs start.
/// This type stores no current language. Share it with Arc across services and
/// keep the resolved locale in each request/recipient job's explicit inputs.
pub struct ServerCatalogs {
    locales: Locales,
    build_id: String,
    message_count: usize,
    catalogs: BTreeMap<Locale, PreparedCatalog>,
}

impl ServerCatalogs {
    pub fn load<'a>(
        locales: Locales,
        build_id: &str,
        message_count: usize,
        artifacts: impl IntoIterator<Item = (Locale, &'a [u8])>,
    ) -> Result<Self, TranslationError> {
        let mut catalogs = BTreeMap::new();
        for (locale, bytes) in artifacts {
            if !locales.supported().any(|configured| configured == &locale) {
                return Err(TranslationError::Initialization(format!(
                    "unexpected host catalog locale {locale}"
                )));
            }
            if catalogs.contains_key(&locale) {
                return Err(TranslationError::Initialization(format!(
                    "duplicate host catalog locale {locale}"
                )));
            }
            let identity = CatalogIdentity::new(
                build_id.into(),
                locale.clone(),
                CatalogAudience::Host,
                message_count,
            )?;
            let catalog = Catalog::load(bytes, &identity)?;
            // Every assigned ID is required on the host (including browser
            // messages for SSR). Missing slots must fail startup, not a job.
            for id in 0..message_count {
                catalog.message(MessageId(id as u32))?;
            }
            catalogs.insert(locale, PreparedCatalog::new(catalog)?);
        }
        for locale in locales.supported() {
            if !catalogs.contains_key(locale) {
                return Err(TranslationError::Initialization(format!(
                    "missing required host catalog for {locale}"
                )));
            }
        }
        Ok(Self {
            locales,
            build_id: build_id.into(),
            message_count,
            catalogs,
        })
    }

    /// Load exact content-addressed compiler outputs from a private directory.
    /// Filenames are basenames, never paths supplied by a request. No files are
    /// read after this returns successfully.
    pub fn load_directory(
        locales: Locales,
        build_id: &str,
        message_count: usize,
        directory: &Path,
        files: &[(Locale, String)],
    ) -> Result<Self, TranslationError> {
        let mut artifacts = Vec::new();
        for (locale, filename) in files {
            let prefix = format!("{locale}.host.");
            let hash = filename
                .strip_prefix(&prefix)
                .and_then(|value| value.strip_suffix(".json"))
                .filter(|hash| {
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
                .ok_or_else(|| {
                    TranslationError::Initialization(format!(
                        "invalid host catalog filename {filename:?}"
                    ))
                })?;
            let read = || -> std::io::Result<Vec<u8>> {
                let file = std::fs::File::open(directory.join(filename))?;
                let mut bytes = Vec::new();
                file.take(crate::catalog::MAX_CATALOG_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                Ok(bytes)
            };
            let bytes = read().map_err(|error| {
                TranslationError::Initialization(format!("cannot read {filename}: {error}"))
            })?;
            if bytes.len() > crate::catalog::MAX_CATALOG_BYTES {
                return Err(crate::CatalogError::TooLarge.into());
            }
            if pocopine_crypto::sha256_hex(&bytes) != hash {
                return Err(TranslationError::Initialization(format!(
                    "host catalog content hash mismatch: {filename}"
                )));
            }
            artifacts.push((locale.clone(), bytes));
        }
        Self::load(
            locales,
            build_id,
            message_count,
            artifacts
                .iter()
                .map(|(locale, bytes)| (locale.clone(), bytes.as_slice())),
        )
    }

    pub fn locales(&self) -> &Locales {
        &self.locales
    }
    pub fn build_id(&self) -> &str {
        &self.build_id
    }
    pub fn message_count(&self) -> usize {
        self.message_count
    }
    pub fn format(
        &self,
        locale: &Locale,
        id: MessageId,
        args: &[(&str, Value<'_>)],
    ) -> Result<String, TranslationError> {
        let effective = self.locales.resolve(locale);
        self.catalogs[effective].format(id, args)
    }
    pub fn render(
        &self,
        locale: &Locale,
        id: MessageId,
        args: &[(&str, Value<'_>)],
    ) -> Result<Vec<crate::RenderedPart>, TranslationError> {
        self.catalogs[self.locales.resolve(locale)].render(id, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CATALOG_FORMAT_VERSION, CatalogArtifact, CatalogEntry};

    fn locales() -> Locales {
        Locales::new(
            "en".parse().unwrap(),
            ["en", "fr"].map(|value| value.parse().unwrap()),
        )
        .unwrap()
    }
    fn artifact(locale: &str, text: &str) -> Vec<u8> {
        serde_json::to_vec(&CatalogArtifact {
            format_version: CATALOG_FORMAT_VERSION,
            build_id: "a".repeat(64),
            locale: locale.parse().unwrap(),
            audience: CatalogAudience::Host,
            messages: vec![Some(CatalogEntry {
                source_locale: locale.parse().unwrap(),
                message: text.into(),
            })],
        })
        .unwrap()
    }
    #[test]
    fn host_initialization_is_complete_before_use_and_has_no_ambient_locale() {
        let en = artifact("en", "Hello {name}");
        let fr = artifact("fr", "Bonjour {name}");
        let inputs = [
            ("en".parse().unwrap(), en.as_slice()),
            ("fr".parse().unwrap(), fr.as_slice()),
        ];
        let catalogs = ServerCatalogs::load(locales(), &"a".repeat(64), 1, inputs.clone()).unwrap();
        let args = [("name", Value::Text("Ari"))];
        assert_eq!(
            catalogs
                .format(&"en".parse().unwrap(), MessageId(0), &args)
                .unwrap(),
            "Hello Ari"
        );
        assert_eq!(
            catalogs
                .format(&"fr-CA".parse().unwrap(), MessageId(0), &args)
                .unwrap(),
            "Bonjour Ari"
        );
        assert_eq!(
            catalogs
                .format(&"ja".parse().unwrap(), MessageId(0), &args)
                .unwrap(),
            "Hello Ari"
        );
        assert!(ServerCatalogs::load(locales(), &"a".repeat(64), 1, [inputs[0].clone()]).is_err());
        assert!(ServerCatalogs::load(locales(), &"b".repeat(64), 1, inputs).is_err());
    }
    #[test]
    fn file_hash_corruption_and_null_host_slots_fail_startup() {
        let dir = tempfile::tempdir().unwrap();
        let en = artifact("en", "Hello");
        let fr = artifact("fr", "Bonjour");
        let files = [
            (
                "en".parse().unwrap(),
                format!("en.host.{}.json", pocopine_crypto::sha256_hex(&en)),
            ),
            (
                "fr".parse().unwrap(),
                format!("fr.host.{}.json", pocopine_crypto::sha256_hex(&fr)),
            ),
        ];
        std::fs::write(dir.path().join(&files[0].1), &en).unwrap();
        std::fs::write(dir.path().join(&files[1].1), &fr).unwrap();
        assert!(
            ServerCatalogs::load_directory(locales(), &"a".repeat(64), 1, dir.path(), &files)
                .is_ok()
        );
        std::fs::write(dir.path().join(&files[1].1), b"{}").unwrap();
        assert!(
            ServerCatalogs::load_directory(locales(), &"a".repeat(64), 1, dir.path(), &files)
                .is_err()
        );
        let mut empty: CatalogArtifact = serde_json::from_slice(&fr).unwrap();
        empty.messages[0] = None;
        let empty = serde_json::to_vec(&empty).unwrap();
        assert!(
            ServerCatalogs::load(
                locales(),
                &"a".repeat(64),
                1,
                [
                    (files[0].0.clone(), en.as_slice()),
                    (files[1].0.clone(), empty.as_slice())
                ]
            )
            .is_err()
        );
    }
}
