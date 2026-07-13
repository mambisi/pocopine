//! Explicit wire compatibility for collaborative documents.
//!
//! The collab transport is generic: it cannot infer whether two arbitrary yrs
//! documents share the same schema or step encoding. Callers therefore provide
//! one [`CompatibilityIdentity`] at both the client and server boundary. The
//! identity is carried in the opening protocol hello and prefixes the realtime
//! topic, which also namespaces fan-out rooms and persistence keys.

use crate::error::{CollabError, CollabResult};

/// Number of lowercase hexadecimal characters in a SHA-256 fingerprint.
pub const FINGERPRINT_HEX_LEN: usize = 64;

/// The application protocol and schema identity required for collaboration.
///
/// `protocol_version` covers the collab/step encoding. `fingerprint` covers the
/// caller's semantic document schema. The fingerprint is deliberately supplied
/// by the caller: `pocopine-collab` remains useful for Pine rich text, canvases,
/// and any other yrs-backed document model without hard-coding one of them.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CompatibilityIdentity {
    protocol_version: u16,
    fingerprint: String,
}

impl CompatibilityIdentity {
    /// Build a validated compatibility identity.
    ///
    /// Fingerprints are canonical lowercase SHA-256 hex. Uppercase input is
    /// rejected rather than normalized so one schema cannot accidentally split
    /// into two room/persistence namespaces.
    pub fn new(protocol_version: u16, fingerprint: impl Into<String>) -> CollabResult<Self> {
        let fingerprint = fingerprint.into();
        if fingerprint.len() != FINGERPRINT_HEX_LEN
            || !fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(CollabError::Compatibility(format!(
                "fingerprint must be exactly {FINGERPRINT_HEX_LEN} lowercase hexadecimal characters"
            )));
        }
        Ok(Self {
            protocol_version,
            fingerprint,
        })
    }

    /// Application collab/step protocol version.
    pub fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    /// Canonical 64-character lowercase schema fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Namespace a logical document key for realtime fan-out and persistence.
    ///
    /// The resulting topic is also the key supplied to [`crate::CollabStore`],
    /// so incompatible schemas can never load or append to the same history.
    pub fn namespace_topic(&self, document_key: &str) -> String {
        format!(
            "collab:v{}:{}:{document_key}",
            self.protocol_version, self.fingerprint
        )
    }

    /// Whether `topic` belongs to this exact protocol/schema namespace.
    pub fn accepts_topic(&self, topic: &str) -> bool {
        let prefix = format!("collab:v{}:{}:", self.protocol_version, self.fingerprint);
        topic
            .strip_prefix(&prefix)
            .is_some_and(|document_key| !document_key.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn validates_and_namespaces_an_identity() {
        let identity = CompatibilityIdentity::new(7, FINGERPRINT).unwrap();
        assert_eq!(identity.protocol_version(), 7);
        assert_eq!(identity.fingerprint(), FINGERPRINT);
        assert_eq!(
            identity.namespace_topic("document-hash"),
            format!("collab:v7:{FINGERPRINT}:document-hash")
        );
        assert!(identity.accepts_topic(&identity.namespace_topic("document-hash")));
        assert!(!identity.accepts_topic(&identity.namespace_topic("")));
    }

    #[test]
    fn rejects_noncanonical_fingerprints() {
        assert!(CompatibilityIdentity::new(1, "short").is_err());
        assert!(
            CompatibilityIdentity::new(
                1,
                "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF"
            )
            .is_err()
        );
        assert!(
            CompatibilityIdentity::new(
                1,
                "g123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .is_err()
        );
    }
}
