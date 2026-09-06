use super::*;
use icu_provider_blob::BlobDataProvider;
use std::sync::OnceLock;

impl Formatter {
    pub(crate) fn new(locale: &Locale) -> Result<Self, RenderError> {
        static DATA: OnceLock<Result<BlobDataProvider, String>> = OnceLock::new();
        let provider = DATA.get_or_init(|| {
            BlobDataProvider::try_new_from_static_blob(include_bytes!(concat!(
                env!("OUT_DIR"),
                "/formatting.blob"
            )))
            .map_err(|e| e.to_string())
        });
        Self::with_provider(provider.as_ref().map_err(error)?, locale)
    }
}
