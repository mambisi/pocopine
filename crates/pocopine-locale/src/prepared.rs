use std::fmt;

use crate::{
    Catalog, CatalogError, Locale, MessageFormatter, MessageId, RenderError, RenderedPart, Value,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TranslationError {
    Catalog(CatalogError),
    Render(RenderError),
    Initialization(String),
    NotInitialized,
    CatalogNotLoaded(Locale),
}
impl fmt::Display for TranslationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(error) => error.fmt(f),
            Self::Render(error) => error.fmt(f),
            Self::Initialization(message) => write!(f, "locale initialization failed: {message}"),
            Self::NotInitialized => f.write_str("translation catalogs are not initialized"),
            Self::CatalogNotLoaded(locale) => {
                write!(f, "translation catalog for {locale} is not loaded")
            }
        }
    }
}
impl std::error::Error for TranslationError {}
impl From<CatalogError> for TranslationError {
    fn from(value: CatalogError) -> Self {
        Self::Catalog(value)
    }
}
impl From<RenderError> for TranslationError {
    fn from(value: RenderError) -> Self {
        Self::Render(value)
    }
}

/// An immutable catalog paired with prepared text formatters. Construct it
/// before making a locale available to a request or committing a UI switch.
pub struct PreparedCatalog {
    catalog: Catalog,
    formatter: MessageFormatter,
}
impl PreparedCatalog {
    pub fn new(catalog: Catalog) -> Result<Self, TranslationError> {
        let formatter = MessageFormatter::new(catalog.identity().locale().clone())?;
        Ok(Self { catalog, formatter })
    }
    pub fn locale(&self) -> &Locale {
        self.catalog.identity().locale()
    }
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }
    /// Fallback message grammar uses the source language; number/date text
    /// uses the requested, supported catalog locale.
    pub fn format(
        &self,
        id: MessageId,
        args: &[(&str, Value<'_>)],
    ) -> Result<String, TranslationError> {
        Ok(self.formatter.format(&self.catalog.parts(id, args)?)?)
    }
    pub fn render(
        &self,
        id: MessageId,
        args: &[(&str, Value<'_>)],
    ) -> Result<Vec<RenderedPart>, TranslationError> {
        Ok(self.formatter.render(&self.catalog.parts(id, args)?)?)
    }
}
