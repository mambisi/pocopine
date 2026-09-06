//! Explicit locales and shared translation semantics for browser and host.
//!
//! Locale values are ordinary inputs. There is no process-global or
//! request-local current language. CLDR selection uses generated rules;
//! ICU4X is the oracle for those rules and the host text formatter. Browser
//! text uses Intl unless strict-parity explicitly opts into ICU4X.

mod catalog;
mod compiled;
mod config;
mod generated;
mod locale;
mod manifest;
mod message;
mod negotiate;
mod plural;
mod prepared;
mod render;
mod routing;

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
pub use compiled::CompiledMessage;
pub use config::{LocaleConfig, RoutingMode};
pub use locale::{InvalidLocale, Locale, Locales};
pub use manifest::{LocaleManifest, TextDirection};
pub use message::{
    ArgumentKind, DateTimeStyle, FormatError, Message, MessageError, MessagePart, NumberStyle,
    StyleLength, Value,
};
pub use negotiate::{
    LOCALE_COOKIE, LOCALE_HEADER, LocalePreferences, LocaleSource, NegotiatedLocale,
};
pub use plural::{CardinalRule, InvalidPluralArg, PluralArg, PluralCategory};
pub use prepared::{PreparedCatalog, TranslationError};
pub use render::{DateTimeArg, MessageFormatter, RenderError, RenderedPart, TimeZone};
pub use routing::{LOCALE_VISITED_COOKIE, LocaleRoute, LocaleRoutes};

/// The vendored CLDR JSON distribution used to generate the rule tables.
pub const CLDR_VERSION: &str = "48.2.0";

/// Include this application's generated `t` module at the crate root.
/// `pocopine build`, `run`, and `dev` supply the matching generated file.
/// Standalone compiler clients may instead include their own generated file.
#[macro_export]
macro_rules! include_translations {
    () => {
        ::core::include!(::core::env!(
            "POCOPINE_LOCALE_RS",
            "translation code must be generated first; build this application with pocopine build, run, or dev"
        ));
    };
}
