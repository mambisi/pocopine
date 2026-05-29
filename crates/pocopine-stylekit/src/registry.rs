//! Typed utility registry (RFC 092 §architecture).
//!
//! Each utility family declares the value type it expects, so the
//! compiler can reject `w-[red]` as a type error and `bg-surafce` as an
//! unknown-token error — both with spans. This v1 skeleton wires the
//! resolution path and a representative slice of families; expanding to
//! the full §6 catalog is phased work.

use crate::diagnostics::{suggest, Diagnostic, Span};
use crate::emit::{escape_selector, Rule};
use crate::parse::ParsedClass;
use crate::tokens::ThemeTokens;
use crate::{Compilation, CompileOptions};

/// The value type an arbitrary `[…]` is validated against (RFC 092 D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssType {
    Length,
    Color,
    Number,
}

impl CssType {
    /// A coarse, deterministic check used for diagnostics. Real
    /// validation will grow per-type grammars; this is enough to catch
    /// `w-[red]` (a Color where a Length is expected).
    fn accepts(self, value: &str) -> bool {
        match self {
            CssType::Length => {
                value == "0"
                    || value.ends_with("px")
                    || value.ends_with("rem")
                    || value.ends_with('%')
                    || value.starts_with("calc(")
                    || value.starts_with("minmax(")
                    || value.starts_with("var(")
            }
            CssType::Color => {
                value.starts_with('#')
                    || value.starts_with("rgb")
                    || value.starts_with("oklch")
                    || value.starts_with("var(")
            }
            CssType::Number => value.parse::<f32>().is_ok(),
        }
    }
}

/// The utility registry. `builtin()` returns the framework-owned set.
#[derive(Debug)]
pub struct Registry {
    _private: (),
}

impl Registry {
    /// The framework-owned registry. v1 skeleton — see the §6 catalog
    /// for the target coverage.
    pub fn builtin() -> Self {
        Self { _private: () }
    }

    /// Resolve + emit one parsed class into `out`, recording any
    /// diagnostic. `literal` is the verbatim class text (authoritative
    /// for the selector).
    pub fn emit_into(
        &self,
        literal: &str,
        parsed: &ParsedClass,
        tokens: &ThemeTokens,
        span: Span,
        options: &CompileOptions,
        out: &mut Compilation,
    ) {
        let declarations = match self.resolve(parsed, tokens) {
            Ok(decls) => decls,
            Err(diag) => {
                // RFC 092 D5: --stylekit-compat=warn downgrades unknown
                // utilities so a half-ported example still builds.
                let diag = if options.compat_warn {
                    Diagnostic {
                        severity: crate::Severity::Warning,
                        ..diag
                    }
                } else {
                    diag
                };
                out.diagnostics.push(diag.at(span));
                return;
            }
        };

        let mut selector = escape_selector(literal);
        let mut at_rule = None;
        for variant in &parsed.variants {
            match resolve_variant(&variant.0) {
                VariantResolution::Pseudo(suffix) => selector.push_str(&suffix),
                VariantResolution::AtRule(at) => at_rule = Some(at),
                VariantResolution::Unknown => {
                    out.diagnostics.push(
                        Diagnostic::error(format!("unknown variant `{}`", variant.0)).at(span),
                    );
                    return;
                }
            }
        }

        out.css.push_str(
            &Rule {
                selector,
                declarations,
                at_rule,
            }
            .render(),
        );
    }

    /// Map a parsed base (+ optional arbitrary value) to CSS
    /// declarations, or a diagnostic.
    fn resolve(
        &self,
        parsed: &ParsedClass,
        tokens: &ThemeTokens,
    ) -> Result<Vec<(String, String)>, Diagnostic> {
        let base = parsed.base.as_str();

        // 1. Static utilities (no value).
        if parsed.arbitrary.is_none() {
            if let Some(decls) = static_utility(base) {
                return Ok(decls);
            }
            // 2. Token-backed color families: `bg-surface`, `text-ink-50`.
            if let Some((family, prop)) = color_family(base) {
                let name = base[family.len() + 1..].to_string(); // strip "bg-"
                return match tokens.var_for("color", &name) {
                    Some(var) => Ok(vec![(prop.to_string(), var)]),
                    None => Err(unknown_token(base, "color", &name, tokens)),
                };
            }
        }

        // 3. Arbitrary value families: `w-[12px]`, `h-[3px]`.
        if let Some(value) = &parsed.arbitrary {
            if let Some((ty, prop)) = arbitrary_family(base) {
                return if ty.accepts(value) {
                    Ok(vec![(prop.to_string(), normalize_arbitrary(value))])
                } else {
                    Err(Diagnostic::error(format!(
                        "`{base}` expects a {ty:?} value, got `{value}`"
                    )))
                };
            }
        }

        // 4. Unknown utility — suggest the nearest known name.
        let mut diag = Diagnostic::error(format!("unknown utility `{base}`"));
        if let Some(hint) = suggest(base, KNOWN_BASES.iter().copied()) {
            diag = diag.with_help(format!("did you mean `{hint}`?"));
        }
        Err(diag)
    }
}

fn unknown_token(base: &str, family: &str, name: &str, tokens: &ThemeTokens) -> Diagnostic {
    let candidates: Vec<String> = tokens
        .names_in_family(family)
        .map(|n| format!("{}-{n}", &base[..base.len() - name.len() - 1]))
        .collect();
    let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
    let mut diag = Diagnostic::error(format!("`--{family}-{name}` is not a defined token"));
    if let Some(hint) = suggest(base, refs) {
        diag = diag.with_help(format!("did you mean `{hint}`?"));
    }
    diag
}

/// Replace Tailwind's `_`-for-space convention inside arbitrary values.
fn normalize_arbitrary(value: &str) -> String {
    value.replace('_', " ")
}

/// Static, value-less utilities. Representative slice of the §6 catalog.
fn static_utility(base: &str) -> Option<Vec<(String, String)>> {
    let decl = |p: &str, v: &str| Some(vec![(p.to_string(), v.to_string())]);
    match base {
        "block" => decl("display", "block"),
        "flex" => decl("display", "flex"),
        "inline-flex" => decl("display", "inline-flex"),
        "grid" => decl("display", "grid"),
        "hidden" => decl("display", "none"),
        "items-center" => decl("align-items", "center"),
        "justify-center" => decl("justify-content", "center"),
        "relative" => decl("position", "relative"),
        "absolute" => decl("position", "absolute"),
        _ => None,
    }
}

/// Token-backed color families → (prefix, CSS property).
fn color_family(base: &str) -> Option<(&'static str, &'static str)> {
    for (prefix, prop) in [
        ("bg", "background-color"),
        ("text", "color"),
        ("border", "border-color"),
    ] {
        if base.len() > prefix.len() + 1
            && base.starts_with(prefix)
            && base.as_bytes()[prefix.len()] == b'-'
        {
            return Some((prefix, prop));
        }
    }
    None
}

/// Arbitrary-value families → (expected type, CSS property).
fn arbitrary_family(base: &str) -> Option<(CssType, &'static str)> {
    match base {
        "w" => Some((CssType::Length, "width")),
        "h" => Some((CssType::Length, "height")),
        "min-w" => Some((CssType::Length, "min-width")),
        "max-w" => Some((CssType::Length, "max-width")),
        _ => None,
    }
}

enum VariantResolution {
    Pseudo(String),
    AtRule(String),
    Unknown,
}

/// Map a variant token to a selector suffix or at-rule wrapper.
fn resolve_variant(variant: &str) -> VariantResolution {
    use VariantResolution::*;
    match variant {
        "hover" => Pseudo(":hover".into()),
        "focus" => Pseudo(":focus".into()),
        "focus-visible" => Pseudo(":focus-visible".into()),
        "active" => Pseudo(":active".into()),
        "disabled" => Pseudo(":disabled".into()),
        "sm" => AtRule("@media (min-width: 640px)".into()),
        "md" => AtRule("@media (min-width: 768px)".into()),
        "lg" => AtRule("@media (min-width: 1024px)".into()),
        "xl" => AtRule("@media (min-width: 1280px)".into()),
        // data-[state=on] / aria-[…] → attribute selector.
        v if v.starts_with("data-[") && v.ends_with(']') => {
            let inner = &v["data-[".len()..v.len() - 1];
            match inner.split_once('=') {
                Some((k, val)) => Pseudo(format!("[data-{k}=\"{val}\"]")),
                None => Pseudo(format!("[data-{inner}]")),
            }
        }
        _ => Unknown,
    }
}

/// Flat list of known base names, for "did you mean" suggestions.
const KNOWN_BASES: &[&str] = &[
    "block",
    "flex",
    "inline-flex",
    "grid",
    "hidden",
    "items-center",
    "justify-center",
    "relative",
    "absolute",
    "bg-surface",
    "w",
    "h",
    "min-w",
    "max-w",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_class;

    fn compile_one(literal: &str, tokens: &ThemeTokens) -> Compilation {
        let mut out = Compilation::default();
        let parsed = parse_class(literal).unwrap();
        Registry::builtin().emit_into(
            literal,
            &parsed,
            tokens,
            Span::UNKNOWN,
            &CompileOptions::default(),
            &mut out,
        );
        out
    }

    #[test]
    fn emits_static_utility() {
        let out = compile_one("flex", &ThemeTokens::new());
        assert!(!out.has_errors());
        assert_eq!(out.css, ".flex {\n  display: flex;\n}\n");
    }

    #[test]
    fn emits_token_backed_color() {
        let mut t = ThemeTokens::new();
        t.insert("color-surface", "#fff");
        let out = compile_one("bg-surface", &t);
        assert!(!out.has_errors());
        assert!(out.css.contains("background-color: var(--color-surface);"));
    }

    #[test]
    fn unknown_token_errors() {
        let t = ThemeTokens::new();
        let out = compile_one("bg-surface", &t);
        assert!(out.has_errors());
    }

    #[test]
    fn arbitrary_type_mismatch_errors() {
        let out = compile_one("w-[red]", &ThemeTokens::new());
        assert!(out.has_errors());
    }

    #[test]
    fn hover_variant_suffix() {
        let mut t = ThemeTokens::new();
        t.insert("color-surface", "#fff");
        let out = compile_one("hover:bg-surface", &t);
        assert!(out.css.starts_with(".hover\\:bg-surface:hover {"));
    }

    #[test]
    fn unknown_utility_suggests() {
        let out = compile_one("flexx", &ThemeTokens::new());
        assert!(out.has_errors());
        assert_eq!(
            out.diagnostics[0].help.as_deref(),
            Some("did you mean `flex`?")
        );
    }
}
