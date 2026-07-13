//! Document identity and access.
//!
//! A [`CollabDoc`] hashes to a stable `doc_hash` (via `pocopine-crypto`) used
//! for a compatibility-namespaced topic on the `pocopine-realtime` gateway and
//! for the per-document Redis keys. The hash input is length-prefixed so distinct
//! field tuples can never collide.

use pocopine_crypto::sha256_hex;

use crate::CompatibilityIdentity;

/// A collaborative document's identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CollabDoc {
    pub namespace: String,
    pub collection: String,
    pub id: String,
    pub branch: String,
}

impl CollabDoc {
    /// Construct a document identity.
    pub fn new(
        namespace: impl Into<String>,
        collection: impl Into<String>,
        id: impl Into<String>,
        branch: impl Into<String>,
    ) -> Self {
        Self {
            namespace: namespace.into(),
            collection: collection.into(),
            id: id.into(),
            branch: branch.into(),
        }
    }

    /// The canonical byte encoding of the identity. Each field is length-
    /// prefixed (u64 LE), so no field value can forge a separator — `{"a","b"}`
    /// and `{"a:b",""}` are guaranteed to encode (and therefore hash)
    /// differently.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for field in [&self.namespace, &self.collection, &self.id, &self.branch] {
            out.extend_from_slice(&(field.len() as u64).to_le_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        out
    }

    /// Lowercase sha256 hex of the canonical identity.
    pub fn doc_hash(&self) -> String {
        sha256_hex(&self.canonical_bytes())
    }

    /// The compatibility-namespaced `pocopine-realtime` topic for this document.
    ///
    /// Fan-out rooms and [`crate::CollabStore`] keys both use this exact value,
    /// so incompatible protocol/schema identities never share update history.
    pub fn topic(&self, compatibility: &CompatibilityIdentity) -> String {
        compatibility.namespace_topic(&self.doc_hash())
    }
}

/// The access a connection has to a document, resolved by the consumer's async
/// authorizer (a superset of the gateway's bool topic policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollabAccess {
    /// May receive updates and publish document updates + awareness.
    ReadWrite,
    /// May receive updates and send awareness, but not publish document
    /// updates.
    ReadOnly,
    /// No access.
    Deny,
}

impl CollabAccess {
    /// May this connection publish document updates?
    pub fn can_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    /// May this connection receive updates and join at all?
    pub fn can_read(self) -> bool {
        matches!(self, Self::ReadWrite | Self::ReadOnly)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_hash_is_stable_and_64_hex_chars() {
        let d = CollabDoc::new("app", "docs", "readme", "main");
        let compatibility = CompatibilityIdentity::new(
            1,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap();
        let h = d.doc_hash();
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            h,
            CollabDoc::new("app", "docs", "readme", "main").doc_hash()
        );
        assert_eq!(
            d.topic(&compatibility),
            format!("collab:v1:{}:{h}", compatibility.fingerprint())
        );
    }

    #[test]
    fn distinct_identities_hash_differently() {
        let a = CollabDoc::new("app", "docs", "readme", "main");
        let b = CollabDoc::new("app", "docs", "readme", "draft");
        assert_ne!(a.doc_hash(), b.doc_hash());
    }

    #[test]
    fn length_prefix_prevents_field_boundary_collisions() {
        // Without length-prefixing these could collide under a ':' separator.
        let a = CollabDoc::new("a", "b", "", "");
        let b = CollabDoc::new("a:b", "", "", "");
        assert_ne!(a.doc_hash(), b.doc_hash());
    }

    #[test]
    fn access_predicates() {
        assert!(CollabAccess::ReadWrite.can_write());
        assert!(CollabAccess::ReadWrite.can_read());
        assert!(!CollabAccess::ReadOnly.can_write());
        assert!(CollabAccess::ReadOnly.can_read());
        assert!(!CollabAccess::Deny.can_read());
    }
}
