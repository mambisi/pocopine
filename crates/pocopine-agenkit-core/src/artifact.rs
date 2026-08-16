//! Client-safe artifact shapes (RFC-122).
//!
//! An [`ArtifactRef`] is a reference to AI-produced bytes captured in
//! implementor-owned storage: everything on it is small by contract, so it is
//! wire- and transcript-safe. Bytes never ride these types — capture happens
//! server-side through the `ArtifactSink` trait in `pocopine-agenkit`, and
//! serving bytes back is the implementor's surface (RFC-122 §1).
//!
//! The reserved `{"$artifact": { ... }}` embedding shape (a sibling of the
//! session layer's `$session_blob`) lets a tool carry a ref inside its
//! ordinary JSON output in a form consumers can recognize and lift.

use serde::{Deserialize, Serialize};

/// A reference to AI-produced bytes captured in implementor storage
/// (RFC-122 §1). Small by contract: every field is wire- and transcript-safe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Implementor-minted opaque id, stable for the artifact's lifetime.
    pub id: String,
    /// Implementor's reference scheme (e.g. `ak:file/<key>`), suitable for
    /// embedding in prose/markdown. `None` when the sink has no addressing
    /// scheme; consumers must then resolve by `id` out of band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// IANA media type of the stored bytes.
    pub media_type: String,
    /// Optional display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// SHA-256 (hex) of the stored bytes.
    pub sha256: String,
    /// Byte length of the stored bytes.
    pub len: u64,
}

/// Multi-output correlation (RFC-122 §5.3): ties the outputs of one producing
/// invocation together so a consumer can slot variants of a single generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaGroupRef {
    /// Runtime-minted group id, shared by every output of the invocation.
    pub id: String,
    /// This output's dense index from 0 (matches the capture-side
    /// `output_ordinal`).
    pub index: u32,
    /// The declared output count when the producer knows it up front
    /// (`n = 4` sampling → four placeholders). Intent, not promise: a
    /// variant can fail while its siblings complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<u32>,
}

/// How a media stream's chunks compose (RFC-122 §5.2). Declared once per
/// stream on [`MediaStarted`](crate::AgentWireEvent::MediaStarted); a
/// consumer's fold is chosen here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WireMediaMode {
    /// Each chunk is a COMPLETE low-fidelity encoding that REPLACES the
    /// previous one (progressive-fidelity image partials). Render the highest
    /// `seq`, discard the rest.
    Preview,
    /// Chunks CONCATENATE in `seq` order into the byte stream (audio/video
    /// live view).
    Append,
    /// Forward-compatibility fallback for a mode this build does not know.
    /// A consumer treats the stream as opaque and waits for the artifact.
    #[serde(other)]
    Unknown,
}

/// Who produced an artifact, on the wire (RFC-122 §3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum WireArtifactOrigin {
    /// The model itself (the RFC-122 §4 backstop).
    Model,
    /// A tool, during dispatch.
    Tool {
        /// The provider's call id (correlates with `ToolStarted`).
        id: String,
        /// The tool's registry id.
        tool: String,
    },
    /// Forward-compatibility fallback for an origin this build does not know.
    #[serde(other)]
    Unknown,
}

/// Byte bound for one `Preview`-mode chunk (base64 length). A chunk over the
/// bound is dropped with a warning, never truncated — a truncated image is
/// garbage, and chunks are ephemeral by contract (RFC-122 §5.2).
pub const MAX_PREVIEW_CHUNK_BYTES: usize = 1024 * 1024;

/// Byte bound for one `Append`-mode chunk (base64 length).
pub const MAX_APPEND_CHUNK_BYTES: usize = 256 * 1024;

/// Default per-stream ephemeral budget for `Append` mode (total base64 bytes
/// across chunks). Past it, further chunks are dropped and consumers wait for
/// the artifact — the agent wire is not a media-delivery protocol
/// (RFC-122 §5.2). Host-tunable at the capture surface.
pub const MAX_APPEND_STREAM_BYTES: usize = 8 * 1024 * 1024;

/// The **reserved** single-key object shape carrying an [`ArtifactRef`]
/// inside ordinary JSON (e.g. a tool's output): `{ "$artifact": { ... } }`.
///
/// Payloads must never legitimately use a top-level `$artifact` key for
/// anything else; on read, a value of exactly this shape may be lifted as an
/// artifact reference (RFC-122 §2).
pub const ARTIFACT_KEY: &str = "$artifact";

/// The reserved embedding form of a ref (the shape consumers lift).
pub fn artifact_ref_value(artifact: &ArtifactRef) -> serde_json::Value {
    serde_json::json!({ ARTIFACT_KEY: artifact })
}

/// Read `value` back as an artifact reference iff it is exactly
/// `{ "$artifact": … }` (single key, well-formed ref).
pub fn as_artifact_ref(value: &serde_json::Value) -> Option<ArtifactRef> {
    let obj = value.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    serde_json::from_value(obj.get(ARTIFACT_KEY)?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ref() -> ArtifactRef {
        ArtifactRef {
            id: "art_1".to_string(),
            uri: Some("ak:file/abc".to_string()),
            media_type: "image/png".to_string(),
            name: Some("chart.png".to_string()),
            sha256: "aa".repeat(32),
            len: 12_345,
        }
    }

    #[test]
    fn artifact_ref_round_trips_and_omits_absent_fields() {
        let full = sample_ref();
        let json = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<ArtifactRef>(&json).unwrap(), full);

        let bare = ArtifactRef {
            uri: None,
            name: None,
            ..full
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(!json.contains("uri"));
        assert!(!json.contains("name"));
        assert_eq!(serde_json::from_str::<ArtifactRef>(&json).unwrap(), bare);
    }

    #[test]
    fn reserved_shape_lifts_only_the_exact_single_key_form() {
        let artifact = sample_ref();
        let value = artifact_ref_value(&artifact);
        assert_eq!(as_artifact_ref(&value), Some(artifact.clone()));

        // A second key breaks the reserved shape — treated as ordinary data.
        let mut with_extra = value.clone();
        with_extra
            .as_object_mut()
            .unwrap()
            .insert("note".to_string(), serde_json::json!("hi"));
        assert_eq!(as_artifact_ref(&with_extra), None);

        // A malformed body under the key is data, not a ref.
        let forged = serde_json::json!({ ARTIFACT_KEY: { "id": 7 } });
        assert_eq!(as_artifact_ref(&forged), None);
    }

    #[test]
    fn media_mode_and_origin_degrade_to_unknown() {
        let mode: WireMediaMode = serde_json::from_str(r#""interleave_v2""#).unwrap();
        assert_eq!(mode, WireMediaMode::Unknown);
        let origin: WireArtifactOrigin =
            serde_json::from_str(r#"{"kind":"scheduler"}"#).unwrap();
        assert_eq!(origin, WireArtifactOrigin::Unknown);
    }

    #[test]
    fn group_ref_expected_is_optional() {
        let group = MediaGroupRef {
            id: "grp_1".to_string(),
            index: 2,
            expected: None,
        };
        let json = serde_json::to_string(&group).unwrap();
        assert!(!json.contains("expected"));
        assert_eq!(serde_json::from_str::<MediaGroupRef>(&json).unwrap(), group);
    }
}
