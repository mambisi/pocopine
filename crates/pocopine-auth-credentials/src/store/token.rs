//! Ephemeral-token records for password-reset and email-verification.

/// What a stored token authorizes when consumed by `take`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum TokenKind {
    /// Password-reset link token. Confirms ownership of the email.
    PasswordReset,
    /// Email-verification link token. Sets `email_verified = true`.
    EmailVerification,
}

/// One in-flight reset / verification token.
///
/// Stored under `sha256(raw_token)` — the raw token only ever
/// lives in the email and the URL the user clicks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenRecord {
    /// User the token belongs to. Indexes back into the
    /// [`crate::store::UserStore`] when the user clicks the link.
    pub user_id: String,
    /// What this token authorizes.
    pub kind: TokenKind,
    /// Expiry, Unix milliseconds.
    pub expires_at_ms: u64,
}

impl TokenRecord {
    /// Build a record.
    pub fn new(user_id: impl Into<String>, kind: TokenKind, expires_at_ms: u64) -> Self {
        Self {
            user_id: user_id.into(),
            kind,
            expires_at_ms,
        }
    }
}
