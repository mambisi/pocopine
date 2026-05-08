//! [`LoginIdValidator`] — pluggable validation + normalization for
//! the user-supplied login identifier.
//!
//! The credentials crate doesn't assume the login key is an email.
//! Apps choose what their users type to sign in:
//!
//! - **Email** ([`EmailValidator`]) — the most common shape and the
//!   default. RFC 5322 isn't fully implemented (good RFC 5322 takes
//!   thousands of lines and breaks against most user-typed real
//!   addresses anyway); the validator does the structural checks
//!   that catch typos and obviously-malicious input. Lowercases for
//!   lookup.
//! - **E.164 phone** ([`E164PhoneValidator`]) — strict `+\d{8,15}`
//!   per the standard. Apps that need to accept formatted input
//!   (`(555) 123-4567`) pre-normalize before passing the string to
//!   the framework.
//! - **Username** ([`UsernameValidator`]) — alphanumeric +
//!   `_` + `-` by default, configurable min/max length, optional
//!   case folding for lookup.
//!
//! Apps with a stranger shape (account numbers with checksum,
//! domain-restricted emails, NRIC) implement [`LoginIdValidator`]
//! themselves.

use std::borrow::Cow;

/// Validate and normalize the user-typed login identifier.
///
/// Two responsibilities:
///
/// 1. **Validate** — reject malformed input before it reaches the
///    store. The framework maps `Err(reason)` to
///    [`crate::CredentialsError::InvalidLoginId`] (`422 Unprocessable
///    Entity`); the `reason` string surfaces in the closed-set
///    error code path, never as user-visible content.
///
/// 2. **Normalize** — produce the canonical lookup form. The
///    framework calls `normalize` on every login attempt and on
///    signup; both write paths and read paths use the normalized
///    string so case differences ("Alice@example.com" vs
///    "alice@example.com") map to one account.
///
/// Implementations MUST be deterministic (the same input always
/// produces the same `Result`) and idempotent (applying `normalize`
/// twice yields the same value) so storage indexes don't drift.
pub trait LoginIdValidator: Send + Sync + 'static {
    /// Validate the **raw** user-submitted identifier (before
    /// normalization). Returns a stable error string on rejection;
    /// the string identifies the failure class (e.g.
    /// `"missing_at_sign"`) and is mapped onto the
    /// [`crate::CredentialsError::InvalidLoginId`] response.
    ///
    /// Implementations should reject:
    /// - Empty values, control characters, whitespace.
    /// - Excessive length (a sane upper bound for the shape).
    /// - Anything provably malformed for the chosen shape.
    fn validate(&self, raw: &str) -> Result<(), &'static str>;

    /// Convert `raw` into the canonical lookup form (lowercase
    /// email, +E.164 phone, lowercase username, …). Default:
    /// borrow as-is. Implementations that allocate (`to_lowercase`,
    /// trim) return `Cow::Owned`.
    fn normalize<'a>(&self, raw: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(raw)
    }
}

// ─── Email ──────────────────────────────────────────────────────────

/// Validates email-shaped login identifiers and lowercases for
/// lookup.
///
/// **Not** a full RFC 5322 implementation — the framework rejects
/// the obvious cases (missing `@`, no domain dot, control chars,
/// too-long values). Apps that need stricter rules wrap or replace
/// this validator. Apps relying on email-deliverability semantics
/// (rejecting `+`-tagged or `.`-folded variants for a given inbox)
/// should layer that on top.
pub struct EmailValidator;

impl LoginIdValidator for EmailValidator {
    fn validate(&self, raw: &str) -> Result<(), &'static str> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("empty");
        }
        if trimmed.len() > 254 {
            return Err("too_long");
        }
        if trimmed.contains(char::is_whitespace) {
            return Err("contains_whitespace");
        }
        if trimmed.bytes().any(|b| b < 0x20 || b == 0x7f) {
            return Err("contains_control_chars");
        }
        let Some(at) = trimmed.find('@') else {
            return Err("missing_at_sign");
        };
        let (local, rest) = trimmed.split_at(at);
        let domain = &rest[1..];
        if local.is_empty() {
            return Err("empty_local_part");
        }
        if domain.is_empty() {
            return Err("empty_domain");
        }
        if !domain.contains('.') {
            return Err("missing_domain_dot");
        }
        Ok(())
    }

    fn normalize<'a>(&self, raw: &'a str) -> Cow<'a, str> {
        let trimmed = raw.trim();
        if trimmed.bytes().any(|b| b.is_ascii_uppercase()) || trimmed.len() != raw.len() {
            Cow::Owned(trimmed.to_ascii_lowercase())
        } else {
            Cow::Borrowed(raw)
        }
    }
}

// ─── E.164 phone ────────────────────────────────────────────────────

/// Validates phone numbers in [E.164](https://en.wikipedia.org/wiki/E.164)
/// canonical form: `+` followed by 8 to 15 digits.
///
/// Apps that accept formatted input (`(555) 123-4567`,
/// `+1 555 123 4567`) MUST pre-normalize the value before the
/// framework sees it — typically in their wasm-side login form
/// handler. The framework's contract is "if the value isn't already
/// E.164, reject it" so storage indexes match unambiguously and the
/// validator never has to guess about regional formatting.
pub struct E164PhoneValidator;

impl LoginIdValidator for E164PhoneValidator {
    fn validate(&self, raw: &str) -> Result<(), &'static str> {
        if !raw.starts_with('+') {
            return Err("missing_plus_prefix");
        }
        let digits = &raw[1..];
        if digits.is_empty() {
            return Err("empty");
        }
        if !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err("contains_non_digit");
        }
        let len = digits.len();
        if len < 8 {
            return Err("too_short");
        }
        if len > 15 {
            return Err("too_long");
        }
        Ok(())
    }
}

// ─── Username ───────────────────────────────────────────────────────

/// Validates username-shaped identifiers (handle, screen name).
/// Defaults: 3..=32 ASCII alphanumeric / `_` / `-` characters,
/// case-folded to lowercase for lookup.
///
/// Apps that want different rules construct one with explicit
/// values:
///
/// ```
/// use pocopine_auth_credentials::UsernameValidator;
/// let v = UsernameValidator { min_len: 4, max_len: 16, lowercase: true };
/// ```
pub struct UsernameValidator {
    pub min_len: usize,
    pub max_len: usize,
    /// When `true`, [`normalize`](LoginIdValidator::normalize)
    /// lowercases for lookup. Display-case preservation in the
    /// app's user record is unaffected — the app stores its own
    /// casing in its `display_name`/`name` field.
    pub lowercase: bool,
}

impl Default for UsernameValidator {
    fn default() -> Self {
        Self {
            min_len: 3,
            max_len: 32,
            lowercase: true,
        }
    }
}

impl LoginIdValidator for UsernameValidator {
    fn validate(&self, raw: &str) -> Result<(), &'static str> {
        if raw.is_empty() {
            return Err("empty");
        }
        let len = raw.chars().count();
        if len < self.min_len {
            return Err("too_short");
        }
        if len > self.max_len {
            return Err("too_long");
        }
        if !raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err("contains_invalid_chars");
        }
        Ok(())
    }

    fn normalize<'a>(&self, raw: &'a str) -> Cow<'a, str> {
        if self.lowercase && raw.bytes().any(|b| b.is_ascii_uppercase()) {
            Cow::Owned(raw.to_ascii_lowercase())
        } else {
            Cow::Borrowed(raw)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_validator_accepts_well_formed_addresses() {
        let v = EmailValidator;
        v.validate("alice@example.com").unwrap();
        v.validate("a.b+tag@sub.example.co.uk").unwrap();
        v.validate("user@domain.io").unwrap();
    }

    #[test]
    fn email_validator_rejects_malformed() {
        let v = EmailValidator;
        assert_eq!(v.validate("").unwrap_err(), "empty");
        assert_eq!(v.validate("not-an-email").unwrap_err(), "missing_at_sign");
        assert_eq!(v.validate("@example.com").unwrap_err(), "empty_local_part");
        assert_eq!(v.validate("alice@").unwrap_err(), "empty_domain");
        assert_eq!(v.validate("alice@nodot").unwrap_err(), "missing_domain_dot");
        assert_eq!(
            v.validate("alice @example.com").unwrap_err(),
            "contains_whitespace"
        );
        assert_eq!(
            v.validate("alice\x07@example.com").unwrap_err(),
            "contains_control_chars"
        );
        let too_long = format!("{}@example.com", "a".repeat(255));
        assert_eq!(v.validate(&too_long).unwrap_err(), "too_long");
    }

    #[test]
    fn email_validator_normalizes_to_lowercase() {
        let v = EmailValidator;
        assert_eq!(v.normalize("Alice@Example.COM"), "alice@example.com");
        // Already lowercase — no allocation expected (Borrowed).
        match v.normalize("alice@example.com") {
            std::borrow::Cow::Borrowed(_) => {}
            std::borrow::Cow::Owned(_) => panic!("should not allocate for already-lowercase"),
        }
    }

    #[test]
    fn e164_validator_accepts_canonical_form() {
        let v = E164PhoneValidator;
        v.validate("+12025551234").unwrap();
        v.validate("+447700900123").unwrap();
        v.validate("+8613800138000").unwrap();
    }

    #[test]
    fn e164_validator_rejects_non_canonical() {
        let v = E164PhoneValidator;
        assert_eq!(
            v.validate("12025551234").unwrap_err(),
            "missing_plus_prefix"
        );
        assert_eq!(v.validate("+").unwrap_err(), "empty");
        assert_eq!(v.validate("+abc").unwrap_err(), "contains_non_digit");
        assert_eq!(v.validate("+1234").unwrap_err(), "too_short");
        assert_eq!(v.validate("+1234567890123456").unwrap_err(), "too_long");
        assert_eq!(
            v.validate("+1 202 555 1234").unwrap_err(),
            "contains_non_digit"
        );
        assert_eq!(
            v.validate("+1-202-555-1234").unwrap_err(),
            "contains_non_digit"
        );
    }

    #[test]
    fn username_validator_default_accepts_simple_handles() {
        let v = UsernameValidator::default();
        v.validate("alice").unwrap();
        v.validate("alice_42").unwrap();
        v.validate("a-b").unwrap();
    }

    #[test]
    fn username_validator_default_rejects_too_short_too_long_and_invalid_chars() {
        let v = UsernameValidator::default();
        assert_eq!(v.validate("ab").unwrap_err(), "too_short");
        assert_eq!(v.validate(&"a".repeat(33)).unwrap_err(), "too_long");
        assert_eq!(
            v.validate("alice@example").unwrap_err(),
            "contains_invalid_chars"
        );
        assert_eq!(
            v.validate("alice space").unwrap_err(),
            "contains_invalid_chars"
        );
    }

    #[test]
    fn username_validator_normalizes_when_configured() {
        let lower = UsernameValidator::default();
        assert_eq!(lower.normalize("Alice"), "alice");
        let preserved = UsernameValidator {
            min_len: 1,
            max_len: 32,
            lowercase: false,
        };
        assert_eq!(preserved.normalize("Alice"), "Alice");
    }
}
