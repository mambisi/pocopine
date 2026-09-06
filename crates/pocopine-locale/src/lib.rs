//! Explicit locales and shared translation semantics for browser and host.
//!
//! Locale values are ordinary inputs. There is no process-global or
//! request-local current language. CLDR selection uses generated rules;
//! ICU4X is the oracle for those rules and the host text formatter. Browser
//! text uses Intl unless strict-parity explicitly opts into ICU4X.

mod catalog;
mod config;
mod generated;
mod locale;
mod message;
mod plural;
mod prepared;
mod render;

#[cfg(any(not(target_arch = "wasm32"), feature = "strict-parity"))]
mod icu;
#[cfg(any(not(target_arch = "wasm32"), feature = "strict-parity"))]
use icu as platform;
#[cfg(target_arch = "wasm32")]
pub mod client;
#[cfg(all(target_arch = "wasm32", not(feature = "strict-parity")))]
use client::intl as platform;

#[cfg(not(target_arch = "wasm32"))]
pub mod server;

pub use catalog::{
    CATALOG_FORMAT_VERSION, Catalog, CatalogArtifact, CatalogAudience, CatalogEntry, CatalogError,
    CatalogIdentity, CatalogMessage, MessageId,
};
pub use config::{LocaleConfig, RoutingMode};
pub use locale::{InvalidLocale, Locale, Locales};
pub use message::{
    ArgumentKind, DateTimeStyle, FormatError, Message, MessageError, MessagePart, NumberStyle,
    StyleLength, Value,
};
pub use plural::{CardinalRule, InvalidPluralArg, PluralArg, PluralCategory};
pub use prepared::{PreparedCatalog, TranslationError};
pub use render::{DateTimeArg, MessageFormatter, RenderError, RenderedPart, TimeZone};

/// The vendored CLDR JSON distribution used to generate the rule tables.
pub const CLDR_VERSION: &str = "48.2.0";
