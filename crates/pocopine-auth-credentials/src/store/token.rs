//! Ephemeral-token records for password-reset and email-verification.

/// Hash a raw token to the `[u8; 32]` key the [`TokenStore`] is keyed on.
///
/// The raw token only ever lives in the email body and the redirect URL;
/// only this SHA-256 digest is persisted, so even a full database leak
/// yields no reusable tokens. Callers issuing or consuming a token MUST
/// key the store on `token_hash(raw)`, never on the raw value.
///
/// [`TokenStore`]: super::TokenStore
pub fn token_hash(raw_token: &str) -> [u8; 32] {
    pocopine_crypto::sha256(raw_token.as_bytes())
}

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
/// Stored under [`token_hash`] of the raw token — the raw token only
/// ever lives in the email and the URL the user clicks.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hash_is_sha256_of_the_raw_token() {
        // Known SHA-256("abc") vector, as raw bytes.
        assert_eq!(
            token_hash("abc"),
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
                0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
                0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn token_hash_is_stable_and_distinct() {
        assert_eq!(token_hash("same"), token_hash("same"));
        assert_ne!(token_hash("a"), token_hash("b"));
    }
}
