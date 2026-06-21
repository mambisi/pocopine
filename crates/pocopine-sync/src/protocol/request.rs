use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::{
    ClientMutation, SYNC_PROTOCOL_V1, StreamParams, SyncCursor, SyncDeviceId, SyncStreamName,
    default_schema_version_one, deserialize_schema_version_default_one,
};

/// Subscription to one stream, optionally narrowed by typed params.
///
/// Wire backwards-compat: this struct accepts both
/// `{ "stream": "name", "params": {...} }` AND a bare `"name"` string,
/// so a pre-RFC-085 client whose `SyncOpenRequest.streams` carried
/// `Vec<SyncStreamName>` continues to deserialize cleanly into
/// `SyncStreamSubscription { stream: "name", params: {} }`.
///
/// **Symmetric serialize:** when `params` is empty we emit the bare
/// string form so a new client talking to an old server (rolling
/// deploy without server-first ordering) keeps working — the
/// pre-RFC-085 server's `Vec<SyncStreamName>` deserializer only
/// accepts strings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncStreamSubscription {
    pub stream: SyncStreamName,
    pub params: StreamParams,
}

impl SyncStreamSubscription {
    pub fn new(stream: SyncStreamName) -> Self {
        Self {
            stream,
            params: BTreeMap::new(),
        }
    }

    pub fn with_params(mut self, params: StreamParams) -> Self {
        self.params = params;
        self
    }
}

impl From<SyncStreamName> for SyncStreamSubscription {
    fn from(stream: SyncStreamName) -> Self {
        Self::new(stream)
    }
}

impl Serialize for SyncStreamSubscription {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.params.is_empty() {
            // Bare-string form. Symmetric with the pre-RFC-085 wire
            // shape, so a new client serializing an unparameterized
            // subscription remains readable by an old server that
            // expects `Vec<SyncStreamName>`.
            self.stream.serialize(serializer)
        } else {
            use serde::ser::SerializeStruct;
            let mut state = serializer.serialize_struct("SyncStreamSubscription", 2)?;
            state.serialize_field("stream", &self.stream)?;
            state.serialize_field("params", &self.params)?;
            state.end()
        }
    }
}

impl<'de> Deserialize<'de> for SyncStreamSubscription {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{Error as DeError, MapAccess, Visitor};
        use std::fmt;

        struct SubVisitor;

        impl<'de> Visitor<'de> for SubVisitor {
            type Value = SyncStreamSubscription;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a stream name string OR an object \
                     `{ \"stream\": <name>, \"params\": {...} }`",
                )
            }

            // Accept the legacy bare-string form.
            fn visit_str<E: DeError>(self, value: &str) -> Result<Self::Value, E> {
                let stream = SyncStreamName::new(value).map_err(DeError::custom)?;
                Ok(SyncStreamSubscription::new(stream))
            }

            fn visit_string<E: DeError>(self, value: String) -> Result<Self::Value, E> {
                let stream = SyncStreamName::new(value).map_err(DeError::custom)?;
                Ok(SyncStreamSubscription::new(stream))
            }

            // Accept the object form with explicit field-name diagnostics
            // (an untagged enum would silently fall back to default-empty
            // params on a typo like "param": instead of "params":).
            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut stream: Option<SyncStreamName> = None;
                let mut params: Option<StreamParams> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "stream" => {
                            if stream.is_some() {
                                return Err(DeError::duplicate_field("stream"));
                            }
                            stream = Some(map.next_value()?);
                        }
                        "params" => {
                            if params.is_some() {
                                return Err(DeError::duplicate_field("params"));
                            }
                            // Treat explicit `null` as empty map so
                            // non-Rust clients (TS / Python) that emit
                            // null for unset fields don't break.
                            params = Some(
                                map.next_value::<Option<StreamParams>>()?
                                    .unwrap_or_default(),
                            );
                        }
                        other => {
                            return Err(DeError::unknown_field(other, &["stream", "params"]));
                        }
                    }
                }
                let stream = stream.ok_or_else(|| DeError::missing_field("stream"))?;
                Ok(SyncStreamSubscription {
                    stream,
                    params: params.unwrap_or_default(),
                })
            }
        }

        deserializer.deserialize_any(SubVisitor)
    }
}

/// Deserializer for a `StreamParams` field that accepts:
///
/// * the field being absent (handled by `#[serde(default)]`),
/// * the field being explicit JSON `null` (TS / Python clients), AND
/// * a normal object.
///
/// Both `missing` and `null` collapse to an empty map. Without this,
/// an explicit `"params": null` would fail with `invalid type: null,
/// expected a map` which a non-Rust client can't distinguish from a
/// network error.
pub(crate) fn deserialize_params_null_as_default<'de, D>(
    deserializer: D,
) -> Result<StreamParams, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<StreamParams>::deserialize(deserializer)?;
    Ok(opt.unwrap_or_default())
}

/// Open one or more streams.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncOpenRequest {
    pub protocol: String,
    #[serde(default)]
    pub client_id: Option<SyncDeviceId>,
    pub streams: Vec<SyncStreamSubscription>,
}

impl SyncOpenRequest {
    pub fn new<I, S>(streams: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<SyncStreamSubscription>,
    {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            client_id: None,
            streams: streams.into_iter().map(Into::into).collect(),
        }
    }

    pub fn client_id(mut self, client_id: SyncDeviceId) -> Self {
        self.client_id = Some(client_id);
        self
    }
}

/// Pull request for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncPullRequest {
    pub protocol: String,
    pub stream: SyncStreamName,
    /// Filter params for the subscription, must match what was sent
    /// on `/open` so the server's cursor + filter view stays
    /// consistent. Empty for unparameterized subscriptions;
    /// `#[serde(default)]` keeps old-client compat. Explicit JSON
    /// `null` is coerced to empty for non-Rust clients (TS, Python).
    #[serde(
        default,
        deserialize_with = "deserialize_params_null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub params: StreamParams,
    pub cursor: Option<SyncCursor>,
    pub limit: u32,
}

impl SyncPullRequest {
    pub fn new(stream: SyncStreamName) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            params: BTreeMap::new(),
            cursor: None,
            limit: 500,
        }
    }

    pub fn cursor(mut self, cursor: Option<SyncCursor>) -> Self {
        self.cursor = cursor;
        self
    }

    /// Attach the subscription's filter params. Must match what was
    /// sent on `/open` for the same `(stream, params)` pair.
    pub fn params(mut self, params: StreamParams) -> Self {
        self.params = params;
        self
    }
}

/// Push request for one stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(serialize = "M: Serialize", deserialize = "M: Deserialize<'de>"))]
pub struct SyncPushRequest<M> {
    pub protocol: String,
    pub stream: SyncStreamName,
    /// Filter params for the subscription this push belongs to. Must
    /// match what was sent on `/open` so the server can authorize +
    /// route mutations to the right filtered view. Empty for
    /// unparameterized subscriptions; `#[serde(default)]` keeps
    /// old-client compat. Explicit JSON `null` is coerced to empty
    /// for non-Rust clients (TS, Python).
    #[serde(
        default,
        deserialize_with = "deserialize_params_null_as_default",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub params: StreamParams,
    #[serde(default)]
    pub mutations: Vec<ClientMutation<M>>,
    /// Application-level schema version the CLIENT encoded these
    /// mutations against. The server compares against
    /// `SyncStreamSource::schema_version()`; when the client is on an
    /// OLDER version, the source's `migrate_payload` is invoked per
    /// mutation. A source that hasn't registered a migrator rejects
    /// each mutation with `SyncError::SchemaMigration`. Defaults to
    /// `1` so an old client that doesn't send the field is treated
    /// as v1; explicit JSON `null` is coerced to the default too,
    /// for clients (TS, Python) that emit `null` for unset fields.
    #[serde(
        default = "default_schema_version_one",
        deserialize_with = "deserialize_schema_version_default_one"
    )]
    pub schema_version: u32,
}

impl<M> SyncPushRequest<M> {
    pub fn new(
        stream: SyncStreamName,
        mutations: impl IntoIterator<Item = ClientMutation<M>>,
    ) -> Self {
        Self {
            protocol: SYNC_PROTOCOL_V1.to_string(),
            stream,
            params: BTreeMap::new(),
            mutations: mutations.into_iter().collect(),
            schema_version: default_schema_version_one(),
        }
    }

    /// Attach the subscription's filter params. Must match what was
    /// sent on `/open` for the same `(stream, params)` pair.
    pub fn params(mut self, params: StreamParams) -> Self {
        self.params = params;
        self
    }

    /// Set the application-level schema version the mutations are
    /// encoded under. Generated client helpers fill this from the
    /// resource's compile-time `SCHEMA_VERSION` constant. `0` is
    /// coerced to `1` (the framework's canonical default) so the
    /// builder cannot accidentally smuggle an out-of-range value
    /// onto the wire — the server's push handler also rejects `0`
    /// defensively in case a raw `SyncPushRequest` is constructed
    /// via struct literal.
    pub fn with_schema_version(mut self, schema_version: u32) -> Self {
        self.schema_version = if schema_version == 0 {
            default_schema_version_one()
        } else {
            schema_version
        };
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SYNC_PROTOCOL_V1;
    use serde_json::Value;

    #[test]
    fn open_request_serializes_typed_client_id() {
        let request = SyncOpenRequest::new([SyncStreamName::new("posts").unwrap()])
            .client_id(SyncDeviceId::new("device_abc").unwrap());

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["client_id"], "device_abc");
    }

    #[test]
    fn open_request_accepts_bare_stream_names_for_backwards_compat() {
        // Pre-RFC-085 clients serialized `streams` as `Vec<SyncStreamName>`,
        // which JSON-encodes as an array of strings. The new wire envelope
        // is `Vec<SyncStreamSubscription>`, but the custom deserializer
        // accepts bare strings too so old clients keep working against new
        // servers without coordination. See `SyncStreamSubscription`'s
        // `Deserialize` impl.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "streams": ["posts", "comments"],
        });
        let request: SyncOpenRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.streams.len(), 2);
        assert_eq!(request.streams[0].stream.as_str(), "posts");
        assert!(request.streams[0].params.is_empty());
        assert_eq!(request.streams[1].stream.as_str(), "comments");
        assert!(request.streams[1].params.is_empty());
    }

    #[test]
    fn open_request_round_trips_subscription_with_params() {
        // New client encodes params as part of each `SyncStreamSubscription`.
        // The deserializer accepts both the wrapped object form AND the
        // bare-string form, but the wrapped form is the canonical wire shape
        // for parametric subscriptions.
        let mut params = BTreeMap::new();
        params.insert("workspace_id".to_string(), Value::String("W".to_string()));
        let request = SyncOpenRequest::new([SyncStreamSubscription {
            stream: SyncStreamName::new("issues").unwrap(),
            params: params.clone(),
        }]);
        let json = serde_json::to_value(&request).unwrap();
        // Serialization keeps the object form when params are non-empty.
        assert_eq!(json["streams"][0]["stream"], "issues");
        assert_eq!(json["streams"][0]["params"]["workspace_id"], "W");

        // And deserialization recovers the same shape.
        let decoded: SyncOpenRequest = serde_json::from_value(json).unwrap();
        assert_eq!(decoded.streams[0].stream.as_str(), "issues");
        assert_eq!(decoded.streams[0].params, params);
    }

    #[test]
    fn open_request_serializes_empty_params_as_bare_string() {
        // Backwards-compat with pre-RFC-085 servers: serialization
        // must emit the BARE STRING form (not an object with no
        // params) when params are empty, so an old server parsing
        // `Vec<SyncStreamName>` continues to deserialize cleanly.
        // Symmetric with `SyncStreamSubscription::Deserialize` which
        // accepts both shapes.
        let request = SyncOpenRequest::new([SyncStreamSubscription {
            stream: SyncStreamName::new("posts").unwrap(),
            params: BTreeMap::new(),
        }]);
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["streams"][0], serde_json::json!("posts"));
    }

    #[test]
    fn open_request_subscription_accepts_explicit_null_params() {
        // Non-Rust clients (TS, Python) often emit `null` for unset
        // map-typed fields. The custom deserializer must coerce
        // `params: null` to an empty map.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "streams": [{"stream": "posts", "params": null}],
        });
        let request: SyncOpenRequest = serde_json::from_value(json).unwrap();
        assert!(request.streams[0].params.is_empty());
    }

    #[test]
    fn open_request_subscription_rejects_unknown_field_in_object_form() {
        // The previous untagged-enum Deserialize silently fell back to
        // empty params on a typo like `param` vs `params`. The custom
        // visitor surfaces an explicit "unknown field" error instead.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "streams": [{"stream": "posts", "param": {"workspace_id": "W"}}],
        });
        let err = serde_json::from_value::<SyncOpenRequest>(json).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field") && msg.contains("param"),
            "expected unknown-field error, got: {msg}"
        );
    }

    #[test]
    fn pull_request_accepts_explicit_null_params() {
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "params": null,
            "cursor": null,
            "limit": 500,
        });
        let request: SyncPullRequest = serde_json::from_value(json).unwrap();
        assert!(request.params.is_empty());
    }

    #[test]
    fn push_request_accepts_explicit_null_params() {
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "params": null,
            "mutations": [],
        });
        let request: SyncPushRequest<Value> = serde_json::from_value(json).unwrap();
        assert!(request.params.is_empty());
    }

    #[test]
    fn pull_request_params_default_empty() {
        // Old servers respond to `/pull` with the legacy envelope shape.
        // A new client deserializing a server response with no `params`
        // field must succeed with an empty map.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "cursor": null,
            "limit": 500,
        });
        let request: SyncPullRequest = serde_json::from_value(json).unwrap();
        assert_eq!(request.stream.as_str(), "posts");
        assert!(request.params.is_empty());
    }

    #[test]
    fn push_request_params_default_empty() {
        // Old client envelope (no params field) deserializes cleanly into
        // the new struct with `params: {}`.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "mutations": [],
        });
        let request: SyncPushRequest<Value> = serde_json::from_value(json).unwrap();
        assert_eq!(request.stream.as_str(), "posts");
        assert!(request.params.is_empty());
    }

    #[test]
    fn push_request_schema_version_accepts_missing_and_explicit_null() {
        let stream = SyncStreamName::new("posts").unwrap();
        // Missing → default 1.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "mutations": [],
        });
        let req: SyncPushRequest<serde_json::Value> = serde_json::from_value(json).unwrap();
        assert_eq!(req.schema_version, 1);
        let _ = stream;
        // Explicit null → coerced to default 1.
        let json = serde_json::json!({
            "protocol": SYNC_PROTOCOL_V1,
            "stream": "posts",
            "mutations": [],
            "schema_version": null,
        });
        let req: SyncPushRequest<serde_json::Value> = serde_json::from_value(json).unwrap();
        assert_eq!(req.schema_version, 1);
    }

    #[test]
    fn with_schema_version_coerces_zero_to_default() {
        let stream = SyncStreamName::new("posts").unwrap();
        let req: SyncPushRequest<serde_json::Value> = SyncPushRequest::new(stream, []);
        let req = req.with_schema_version(0);
        // Builder coerces 0 → 1 so a stray default never lands on the wire.
        assert_eq!(req.schema_version, 1);
    }
}
