use serde::{Deserialize, Serialize};

use crate::{InvalidLocale, Locale, Locales};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingMode {
    #[default]
    PrefixExceptDefault,
    PrefixAll,
    None,
}

/// The `[locale]` section of `pocopine.toml`. Both compilation and runtime
/// configuration validate this set before resolving any user preference.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocaleConfig {
    pub default: Locale,
    pub locales: Vec<Locale>,
    #[serde(default)]
    pub routing: RoutingMode,
    #[serde(default)]
    pub strict_parity: bool,
}

impl LocaleConfig {
    pub fn validate(&self) -> Result<Locales, InvalidLocale> {
        Locales::new(self.default.clone(), self.locales.clone())
    }
}
