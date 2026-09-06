//! Explicit locales and shared translation semantics for browser and host.
//!
//! Locale values are ordinary inputs. There is no process-global or
//! request-local current language. CLDR selection uses generated rules;
//! ICU4X is only a host-side test oracle for those rules.

mod catalog;
mod config;
mod generated;
mod locale;
mod message;
mod plural;

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

/// The vendored CLDR JSON distribution used to generate the rule tables.
pub const CLDR_VERSION: &str = "48.2.0";
