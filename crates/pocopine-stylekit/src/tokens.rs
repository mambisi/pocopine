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

    /// Parse the token model from app CSS (RFC 092 D4 — CSS is the
    /// single source of truth).
    ///
    /// Reads every `@theme { … }` / `@theme inline { … }` block,
    /// collecting `--name: value;` custom-property declarations. Block
    /// comments are stripped first; later blocks override earlier ones,
    /// matching CSS cascade. Non-`@theme` CSS (`@import`, selectors) is
    /// ignored. Parsing is lenient — malformed declarations are skipped
    /// rather than failing the build; structural diagnostics belong to
    /// the extractor, not the token reader.
    pub fn from_css(css: &str) -> Self {
        let mut tokens = Self::new();
        let stripped = strip_block_comments(css);
        for block in theme_blocks(&stripped) {
            for decl in block.split(';') {
                let decl = decl.trim();
                let Some((name, value)) = decl.split_once(':') else {
                    continue;
                };
                let (name, value) = (name.trim(), value.trim());
                if let Some(key) = name.strip_prefix("--") {
                    if !key.is_empty() && !value.is_empty() {
                        tokens.insert(key, value);
                    }
                }
            }
        }
        tokens
    }

    /// Insert/override a token (key without the leading `--`).
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.tokens.insert(key.into(), value.into());
    }

    /// Number of tokens in the model.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether the model holds no tokens.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
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

/// Replace `/* … */` block comments with spaces, preserving newlines so
/// byte offsets (and thus future spans) stay aligned.
fn strip_block_comments(css: &str) -> String {
    let bytes = css.as_bytes();
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                out.push(if bytes[i] == b'\n' { '\n' } else { ' ' });
                i += 1;
            }
            i += 2; // skip closing */
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Yield the inner text of each `@theme { … }` block (comment-stripped
/// input). Brace-depth aware so nested `{ }` don't end the block early.
fn theme_blocks(css: &str) -> Vec<&str> {
    let mut blocks = Vec::new();
    let mut search = css;
    while let Some(at) = search.find("@theme") {
        let after = &search[at + "@theme".len()..];
        let Some(open_rel) = after.find('{') else {
            break;
        };
        let inner_start = open_rel + 1;
        let mut depth = 1i32;
        let mut end = None;
        for (i, b) in after[inner_start..].bytes().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(inner_start + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        match end {
            Some(end) => {
                blocks.push(&after[inner_start..end]);
                search = &after[end + 1..];
            }
            None => break, // unterminated block — stop
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_theme_block_from_css() {
        let css = r#"
            @import "pine-stylekit";

            /* design tokens — warm neutral surface */
            @theme {
              --color-surface: #ffffff;
              --color-ink-100: #18171a;   /* near-black */
              --spacing: 0.25rem;
              --shadow-card: 0 1px 2px rgba(20, 18, 28, 0.05);
            }

            pine-upload-root { display: block; }
        "#;
        let t = ThemeTokens::from_css(css);
        assert_eq!(
            t.var_for("color", "surface").as_deref(),
            Some("var(--color-surface)")
        );
        assert_eq!(
            t.var_for("color", "ink-100").as_deref(),
            Some("var(--color-ink-100)")
        );
        // Value with commas/parens survives intact.
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn later_blocks_override_earlier() {
        let css = "@theme { --color-accent: blue; } @theme inline { --color-accent: red; }";
        let t = ThemeTokens::from_css(css);
        // Both blocks read; second wins. Lookup only checks existence,
        // so assert the underlying value via the manifest.
        assert!(t.to_manifest_json().contains("\"color-accent\": \"red\""));
    }

    #[test]
    fn ignores_commented_out_tokens() {
        let css = "@theme { /* --color-ghost: #000; */ --color-real: #fff; }";
        let t = ThemeTokens::from_css(css);
        assert!(t.var_for("color", "real").is_some());
        assert!(t.var_for("color", "ghost").is_none());
    }

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
