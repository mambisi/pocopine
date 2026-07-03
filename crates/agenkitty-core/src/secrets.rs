//! The one shared secret-content classifier (F3).
//!
//! A conservative heuristic that flags text carrying credential material —
//! bearer tokens, PEM/private-key blocks, and `key = value` / `key: value`
//! assignments to sensitive names. It is deliberately *content*-only and
//! wasm-safe (pure string ops, no I/O), so every tool shares one predicate
//! instead of forking its own:
//!
//! - **memory** and **artifacts** *reject* content that matches (secrets must
//!   never land in a durable agent store);
//! - the **session** redactor replaces matching text with `[redacted]` before
//!   it persists — which also covers every other tool (fs / patch / process /
//!   network) transitively, because their tool-result output is redacted
//!   through the session redactor on its way into the event log.
//!
//! It is a heuristic, not a guarantee: it errs toward catching common
//! credential shapes without flagging ordinary prose or code. Path-based
//! secret-file policy (`.env`, `.aws/credentials`, …) is a *separate* concern
//! handled by the fs tools; this classifier never sees paths, only content.

/// Whether `text` looks like it carries credential material.
pub fn looks_like_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("bearer ")
        || lower.contains("private key")
        || lower.contains("-----begin ")
        || contains_sensitive_assignment(&lower)
}

/// Whether `bytes` — when they are valid UTF-8 — look like credential
/// material. Non-UTF-8 (true binary) is never flagged: the heuristic is a
/// text scanner, and a binary artifact/download can't be inspected this way.
pub fn body_looks_like_secret(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(looks_like_secret)
}

/// The sensitive assignment targets. A match requires the name to sit at a
/// word boundary and be immediately followed (ignoring spaces) by `=` or `:`,
/// so `token = …` matches but `tokenizer` or a bare mention of "password" in
/// prose does not.
const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "x-api-key",
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "authorization",
    "password",
    "secret",
    "client_secret",
    "secret_key",
    "credential",
    "credentials",
    "private_key",
];

fn contains_sensitive_assignment(lower_text: &str) -> bool {
    for key in SENSITIVE_KEYS {
        let mut offset = 0;
        while let Some(position) = lower_text[offset..].find(key) {
            let start = offset + position;
            let end = start + key.len();
            if is_assignment_boundary(lower_text, start, end) {
                return true;
            }
            offset = end;
        }
    }
    false
}

fn is_assignment_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    if before.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
        return false;
    }
    let after = text[end..].trim_start();
    after.starts_with('=') || after.starts_with(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_common_credential_shapes() {
        assert!(looks_like_secret("Authorization: Bearer sk-live-123"));
        assert!(looks_like_secret("api_key = sk-live-12345"));
        assert!(looks_like_secret("password: hunter2"));
        assert!(looks_like_secret("-----BEGIN RSA PRIVATE KEY-----"));
        assert!(looks_like_secret("here is my private key material"));
        // The session-superset keys are now covered everywhere.
        assert!(looks_like_secret("client_secret=abc"));
        assert!(looks_like_secret("credential: xyz"));
        assert!(looks_like_secret("secret_key = zzz"));
    }

    #[test]
    fn does_not_flag_ordinary_prose_or_code() {
        assert!(!looks_like_secret("The tokenizer splits on whitespace."));
        assert!(!looks_like_secret("We chose yrs over a hand-rolled CRDT."));
        assert!(!looks_like_secret("let password_field = form.get(\"pw\");"));
        assert!(!looks_like_secret("api_key is required for this endpoint"));
        assert!(!looks_like_secret(""));
    }

    #[test]
    fn assignment_boundary_requires_a_word_boundary() {
        // `token` inside `tokenizer=` must not match; a real `token=` must.
        assert!(!looks_like_secret("tokenizer=simple"));
        assert!(looks_like_secret("token=abc123"));
    }

    #[test]
    fn body_variant_skips_binary() {
        assert!(body_looks_like_secret(b"api_key = sk-live-1"));
        assert!(!body_looks_like_secret(b"plain report text"));
        assert!(!body_looks_like_secret(&[0xff, 0xfe, 0x00, 0x01]));
    }
}
