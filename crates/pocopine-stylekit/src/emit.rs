//! Deterministic CSS emission + selector escaping (RFC 092 §architecture).
//!
//! A utility class like `hover:bg-surface` becomes a CSS rule whose
//! selector escapes the class-literal special characters and appends
//! the variant pseudo-classes / wraps in at-rules.

/// Escape a class name for use in a CSS selector. The literal class text
/// (`hover:bg-[12px]`, `data-[active=true]:bg-surface`) appears verbatim in
/// the DOM `class` attribute, so every character that is **not** a valid CSS
/// identifier character — letters, digits, `-`, `_`, and non-ASCII — is
/// backslash-escaped. This covers `:`, `[`, `]`, `.`, `/`, `%`, `(`, `)`,
/// `,`, `#`, `@`, `!`, `=`, `>`, `&`, `*`, whitespace, and anything else a
/// Tailwind-style arbitrary value or variant can carry. An allowlist of
/// "chars to escape" silently drops styles the moment a class uses a
/// character that isn't on it (e.g. the `=` in `data-[active=true]:` made
/// the whole selector an invalid parse, so the rule never applied).
pub fn escape_selector(class: &str) -> String {
    let mut out = String::with_capacity(class.len() + 8);
    for ch in class.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || !ch.is_ascii() {
            out.push(ch);
        } else {
            out.push('\\');
            out.push(ch);
        }
    }
    out
}

/// A single CSS declaration block to emit.
#[derive(Debug, Clone)]
pub struct Rule {
    /// The complete, escaped selector — `render` emits it verbatim. It
    /// already includes the class's leading `.`, any pseudo suffix, and
    /// any ancestor/sibling combinator prefix (so conditioned variants
    /// like `.group:hover .foo` render correctly).
    pub selector: String,
    /// `property: value` pairs, in declaration order.
    pub declarations: Vec<(String, String)>,
    /// Optional at-rule wrapper, e.g. `@media (min-width: 768px)` for a
    /// responsive variant. `None` for top-level rules.
    pub at_rule: Option<String>,
}

impl Rule {
    /// Render to deterministic, minimal CSS.
    pub fn render(&self) -> String {
        let body: String = self
            .declarations
            .iter()
            .map(|(p, v)| format!("  {p}: {v};\n"))
            .collect();
        let rule = format!("{} {{\n{body}}}\n", self.selector);
        match &self.at_rule {
            Some(at) => format!("{at} {{\n{}}}\n", indent(&rule)),
            None => rule,
        }
    }
}

fn indent(s: &str) -> String {
    s.lines().map(|l| format!("  {l}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_variant_and_arbitrary() {
        assert_eq!(escape_selector("hover:bg-surface"), "hover\\:bg-surface");
        assert_eq!(escape_selector("text-[13px]"), "text-\\[13px\\]");
    }

    #[test]
    fn escapes_equals_in_data_attribute_variant() {
        // The `=` must be escaped — an unescaped `=` in the class portion is
        // an invalid CSS selector, so the browser drops the rule and the
        // stateful style (active/current/state) silently never applies.
        assert_eq!(
            escape_selector("data-[active=true]:bg-surface"),
            "data-\\[active\\=true\\]\\:bg-surface"
        );
    }

    #[test]
    fn keeps_identifier_chars_but_escapes_the_rest() {
        // Letters, digits, `-`, `_` pass through; arbitrary-value punctuation
        // is escaped (matches the previously-working `(`, `,`, `.`, `/` cases).
        assert_eq!(
            escape_selector("grid-cols-[minmax(0,1fr)_2rem]"),
            "grid-cols-\\[minmax\\(0\\,1fr\\)_2rem\\]"
        );
    }

    #[test]
    fn renders_simple_rule() {
        let r = Rule {
            selector: ".flex".into(),
            declarations: vec![("display".into(), "flex".into())],
            at_rule: None,
        };
        assert_eq!(r.render(), ".flex {\n  display: flex;\n}\n");
    }
}
