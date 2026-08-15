//! RFC-116 — the type produced by the `poco!` macro.
//!
//! A [`PocoTemplate`] is a zero-cost newtype over the verbatim template
//! source. It exists so template-consuming APIs can demand *proof* that the
//! string was parsed and validated at compile time rather than accepting an
//! arbitrary `&str`, and so the payload can gain precomputed metadata later
//! without breaking callers.
//!
//! Every constructor is `const`, so a template is usable in const position:
//!
//! ```ignore
//! const CARD: PocoTemplate = poco! { <div class="card"></div> };
//! ```

/// A compile-time validated `.poco` template.
///
/// Construct with the `poco!` macro; there is deliberately no public
/// unchecked constructor, because the type's only job is to certify that
/// `pocopine-template-parser` accepted the source at expansion time.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PocoTemplate(&'static str);

impl PocoTemplate {
    /// Expansion plumbing for `poco!`. Not public API: calling this by hand
    /// forges the validation the type is supposed to certify.
    #[doc(hidden)]
    pub const fn __new(source: &'static str) -> Self {
        Self(source)
    }

    /// The template source, verbatim.
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl core::ops::Deref for PocoTemplate {
    type Target = str;

    fn deref(&self) -> &str {
        self.0
    }
}

impl core::fmt::Display for PocoTemplate {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.0)
    }
}

impl AsRef<str> for PocoTemplate {
    fn as_ref(&self) -> &str {
        self.0
    }
}

impl From<PocoTemplate> for &'static str {
    fn from(template: PocoTemplate) -> Self {
        template.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: PocoTemplate = PocoTemplate::__new("<div>x</div>");

    #[test]
    fn usable_in_const_position_and_derefs_to_str() {
        assert_eq!(FIXTURE.as_str(), "<div>x</div>");
        assert!(FIXTURE.starts_with("<div>"));
        assert_eq!(FIXTURE.to_string(), "<div>x</div>");
    }
}
