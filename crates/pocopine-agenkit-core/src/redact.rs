//! Wire-boundary redaction (§D10) — the shared helpers every UI bridge uses to
//! map trusted host-side data onto a client-facing surface, instead of each
//! consumer hand-rolling its own (redaction-by-construction).
//!
//! A [`Redactor`] bounds text/JSON payload sizes, blanks values under
//! credential-shaped keys ([`is_sensitive_key`]), and — when the host plugs in
//! a secret-content classifier — fully redacts any string the classifier
//! flags. The classifier is pluggable because content classification is host
//! policy (e.g. agenkitty's shared `looks_like_secret`), while the structural
//! rules here are universal.

use std::sync::Arc;

use serde_json::{Map, Value};

/// The replacement marker for redacted content.
pub const REDACTED: &str = "[redacted]";

/// Nesting depth beyond which a JSON value is replaced instead of walked (a
/// guard against adversarially deep payloads).
const MAX_JSON_REDACTION_DEPTH: usize = 64;

/// A pluggable secret-content classifier (see
/// [`secret_classifier`](Redactor::secret_classifier)).
type SecretClassifier = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// A reusable text/JSON redaction policy for one wire boundary.
///
/// Defaults: text capped at 4096 bytes, JSON strings at 2048 bytes, values
/// under credential-shaped keys blanked, no content classifier. Plug one in
/// with [`secret_classifier`](Redactor::secret_classifier) to also fully
/// redact any string whose *content* looks like a secret.
#[derive(Clone, Default)]
pub struct Redactor {
    text_limit: Option<usize>,
    json_string_limit: Option<usize>,
    is_secret: Option<SecretClassifier>,
}

impl Redactor {
    /// The default policy (4096-byte text, 2048-byte JSON strings, no
    /// content classifier).
    pub fn new() -> Self {
        Self::default()
    }

    /// Override the byte cap applied by [`text`](Redactor::text).
    pub fn text_limit(mut self, bytes: usize) -> Self {
        self.text_limit = Some(bytes);
        self
    }

    /// Override the byte cap applied to each JSON string by
    /// [`json`](Redactor::json).
    pub fn json_string_limit(mut self, bytes: usize) -> Self {
        self.json_string_limit = Some(bytes);
        self
    }

    /// Plug in a secret-content classifier: any string it flags is replaced
    /// with `[redacted]` wholesale (not merely truncated).
    pub fn secret_classifier(
        mut self,
        classify: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.is_secret = Some(Arc::new(classify));
        self
    }

    /// Whether the plugged-in secret classifier flags `text` (`false` when no
    /// classifier is configured). Exposed for stateful stream adapters that
    /// must classify *assembled* text across fragments — per-fragment
    /// classification alone misses a secret split across chunk boundaries.
    pub fn flags_secret(&self, text: &str) -> bool {
        self.is_secret.as_ref().is_some_and(|f| f(text))
    }

    /// Redact free text: fully replaced if the classifier flags it, else
    /// truncated to the configured text limit.
    pub fn text(&self, text: &str) -> String {
        self.text_to_limit(text, self.text_limit.unwrap_or(4096))
    }

    /// [`text`](Redactor::text) with an explicit byte cap (for callers with
    /// per-field limits, e.g. a short title vs a long body).
    pub fn text_to_limit(&self, text: &str, max_bytes: usize) -> String {
        if self.flags_secret(text) {
            return REDACTED.to_string();
        }
        truncate_utf8(text, max_bytes)
    }

    /// Redact a JSON payload: values under credential-shaped keys are blanked,
    /// every string is classifier-checked and capped at the configured JSON
    /// string limit, and pathologically deep values are cut off.
    pub fn json(&self, value: &Value) -> Value {
        self.json_to_limit(value, self.json_string_limit.unwrap_or(2048))
    }

    /// [`json`](Redactor::json) with an explicit per-string byte cap.
    pub fn json_to_limit(&self, value: &Value, max_string_bytes: usize) -> Value {
        self.json_at_depth(value, max_string_bytes, 0)
    }

    fn json_at_depth(&self, value: &Value, max_string_bytes: usize, depth: usize) -> Value {
        if depth > MAX_JSON_REDACTION_DEPTH {
            return Value::String("[redacted: too deep]".to_string());
        }
        match value {
            Value::String(text) => Value::String(self.text_to_limit(text, max_string_bytes)),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| self.json_at_depth(item, max_string_bytes, depth + 1))
                    .collect(),
            ),
            Value::Object(object) => {
                let mut redacted = Map::new();
                for (key, value) in object {
                    let value = if is_sensitive_key(key) {
                        Value::String(REDACTED.to_string())
                    } else {
                        self.json_at_depth(value, max_string_bytes, depth + 1)
                    };
                    redacted.insert(key.clone(), value);
                }
                Value::Object(redacted)
            }
            _ => value.clone(),
        }
    }
}

impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redactor")
            .field("text_limit", &self.text_limit)
            .field("json_string_limit", &self.json_string_limit)
            .field("has_secret_classifier", &self.is_secret.is_some())
            .finish()
    }
}

/// Whether a JSON object key names credential material (`api_key`, `token`,
/// `password`, …). Matched on the key's alphanumerics, case-insensitively, so
/// `X-Api-Key`, `apiKey` and `api_key` all count.
pub fn is_sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>();
    matches!(
        normalized.as_str(),
        "apikey"
            | "xapikey"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "authorization"
            | "auth"
            | "password"
            | "passwd"
            | "pwd"
            | "secret"
            | "clientsecret"
            | "secretkey"
            | "privatekey"
            | "credential"
            | "credentials"
    ) || normalized.ends_with("apikey")
        || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
}

/// Truncate `text` to at most `max_bytes` bytes on a char boundary, appending
/// `…` when anything was cut.
pub fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_truncates_on_char_boundaries() {
        let redactor = Redactor::new().text_limit(4);
        // "héllo" = h(1) é(2) l(1)… — byte 4 falls inside a char? h=1,é=2..3,l=4
        assert_eq!(redactor.text("héllo"), "hél…");
        assert_eq!(redactor.text("hey"), "hey");
    }

    #[test]
    fn classifier_fully_redacts_flagged_text() {
        let redactor = Redactor::new().secret_classifier(|text| text.contains("sk-live"));
        assert_eq!(redactor.text("token sk-live-123"), REDACTED);
        assert_eq!(redactor.text("plain"), "plain");
    }

    #[test]
    fn json_blanks_sensitive_keys_and_walks_nested_values() {
        let redactor = Redactor::new().secret_classifier(|text| text.contains("hunter2"));
        let value = serde_json::json!({
            "api_key": "abc",
            "nested": { "Authorization": "Bearer x", "note": "hunter2 is my password" },
            "list": ["ok", { "refresh_token": 1 }],
            "count": 3,
        });
        let redacted = redactor.json(&value);
        assert_eq!(redacted["api_key"], REDACTED);
        assert_eq!(redacted["nested"]["Authorization"], REDACTED);
        assert_eq!(redacted["nested"]["note"], REDACTED);
        assert_eq!(redacted["list"][1]["refresh_token"], REDACTED);
        assert_eq!(redacted["list"][0], "ok");
        assert_eq!(redacted["count"], 3);
    }

    #[test]
    fn json_cuts_off_pathological_depth() {
        let mut value = serde_json::json!("leaf");
        for _ in 0..80 {
            value = serde_json::json!([value]);
        }
        let redacted = Redactor::new().json(&value);
        assert!(
            serde_json::to_string(&redacted)
                .unwrap()
                .contains("[redacted: too deep]")
        );
    }

    #[test]
    fn sensitive_key_matches_are_format_insensitive() {
        for key in ["X-Api-Key", "apiKey", "ACCESS_TOKEN", "my_client_secret"] {
            assert!(is_sensitive_key(key), "{key}");
        }
        for key in ["name", "tokens_used", "content"] {
            assert!(!is_sensitive_key(key), "{key}");
        }
    }
}
