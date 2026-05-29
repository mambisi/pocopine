//! CSS-first theme/token model (RFC 092 D4).
//!
//! Tokens are the single source of truth and live in app CSS:
//!
//! ```css
//! @theme {
//!   --color-surface: #ffffff;
//!   --color-ink-100: #18171a;
//!   --spacing: 0.25rem;
//! }
//! ```
//!
//! The compiler reads `@theme`, exposes the tokens to utility
//! validation + CSS output, and can emit a *generated* JSON manifest
//! for diagnostics/LSP — a derived artifact, never hand-authored.

use std::collections::BTreeMap;

use serde::Serialize;

/// The resolved token model parsed from one or more `@theme` blocks.
///
/// Keys are the custom-property names *without* the leading `--`
/// (`color-surface`, `spacing`). `BTreeMap` keeps output deterministic.
#[derive(Debug, Default, Clone, Serialize)]
pub struct ThemeTokens {
    tokens: BTreeMap<String, String>,
}

impl ThemeTokens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert/override a token (key without the leading `--`).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.tokens.insert(key.into(), value.into());
    }

    /// Look up a token in a family, e.g. `get("color", "surface")` reads
    /// `--color-surface`. Returns the `var(--…)` reference the emitter
    /// should use, or `None` if the token is undefined.
    pub fn var_for(&self, family: &str, name: &str) -> Option<String> {
        let key = format!("{family}-{name}");
        self.tokens
            .contains_key(&key)
            .then(|| format!("var(--{key})"))
    }

    /// All token names within a family (e.g. every `color-*`), for
    /// "unknown token" diagnostics that list valid options.
    pub fn names_in_family<'a>(&'a self, family: &'a str) -> impl Iterator<Item = &'a str> {
        let prefix = format!("{family}-");
        self.tokens
            .keys()
            .filter_map(move |k| k.strip_prefix(&prefix))
    }

    /// Serialize the derived manifest (RFC 092 D4). Stable ordering via
    /// `BTreeMap`, so output is reproducible across builds.
    pub fn to_manifest_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_lookup_and_listing() {
        let mut t = ThemeTokens::new();
        t.insert("color-surface", "#fff");
        t.insert("color-ink-100", "#18171a");
        assert_eq!(
            t.var_for("color", "surface").as_deref(),
            Some("var(--color-surface)")
        );
        assert_eq!(t.var_for("color", "nope"), None);
        let mut names: Vec<_> = t.names_in_family("color").collect();
        names.sort_unstable();
        assert_eq!(names, vec!["ink-100", "surface"]);
    }
}
