//! `SecretString` — an in-memory secret value, redacted and zeroized.
//!
//! The workspace's one primitive for "bytes that are a secret": redacted from
//! `Debug`/`Display` so it can't be logged or panicked into a message, and
//! zeroized on drop so it doesn't linger in freed memory. The value is reachable
//! only through [`SecretString::expose`], so every read is a greppable,
//! intentional use of a secret (header injection, token exchange, …).
//!
//! Lives here (the crypto crate) because secrets are consumed by multiple
//! layers — provider credentials, OAuth tokens, password flows — and the type
//! must sit below all of them. `no_std` (only `alloc` for the `String`).

use alloc::string::String;

use zeroize::Zeroizing;

/// A secret string: redacted from `Debug`/`Display`, zeroized on drop. Read it
/// only via [`SecretString::expose`].
#[derive(Clone)]
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    /// Wrap a secret value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(Zeroizing::new(value.into()))
    }

    /// Borrow the secret. The only way to read it — keep call sites few and
    /// obvious.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl core::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

impl core::fmt::Display for SecretString {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_debug_and_display_but_exposes_value() {
        let s = SecretString::new("sk-live-DO-NOT-LEAK");
        assert_eq!(alloc::format!("{s:?}"), "SecretString(<redacted>)");
        assert_eq!(alloc::format!("{s}"), "<redacted>");
        assert!(!alloc::format!("{s:?}").contains("LEAK"));
        // The value is reachable only through `expose`.
        assert_eq!(s.expose(), "sk-live-DO-NOT-LEAK");
    }

    #[test]
    fn clones_independently() {
        let a = SecretString::new("secret");
        let b = a.clone();
        assert_eq!(a.expose(), b.expose());
    }
}
