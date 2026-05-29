//! Deterministic CSS emission + selector escaping (RFC 092 §architecture).
//!
//! A utility class like `hover:bg-surface` becomes a CSS rule whose
//! selector escapes the class-literal special characters and appends
//! the variant pseudo-classes / wraps in at-rules.

/// Escape a class name for use in a CSS selector. The literal class
/// text (`hover:bg-[12px]`) appears verbatim in the DOM `class`
/// attribute, so the selector must escape `:`, `[`, `]`, `.`, `/`,
/// `%`, `(`, `)`, `,`, `#`, and whitespace with a backslash.
pub fn escape_selector(class: &str) -> String {
    let mut out = String::with_capacity(class.len() + 8);
    for ch in class.chars() {
        match ch {
            ':' | '[' | ']' | '.' | '/' | '%' | '(' | ')' | ',' | '#' | '@' | '!' | ' ' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

/// A single CSS declaration block to emit.
#[derive(Debug, Clone)]
pub struct Rule {
    /// Escaped, fully-qualified selector (already includes the leading
    /// `.` and any pseudo-class suffix).
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
        let rule = format!(".{} {{\n{body}}}\n", self.selector);
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
    fn renders_simple_rule() {
        let r = Rule {
            selector: "flex".into(),
            declarations: vec![("display".into(), "flex".into())],
            at_rule: None,
        };
        assert_eq!(r.render(), ".flex {\n  display: flex;\n}\n");
    }
}
