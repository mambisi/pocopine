//! Schema descriptors shared by tools, flows, and agents (RFC-093 Phase 1.3).
//!
//! A [`SchemaRef`] names a typed payload and optionally carries its JSON
//! schema. It is metadata only — it never carries values or credentials, so
//! it is safe to ship to generated clients.

/// A reference to a typed input/output schema.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SchemaRef {
    /// Stable schema name (typically the Rust type name).
    pub name: String,
    /// Optional JSON-schema description of the shape.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<serde_json::Value>,
}

impl SchemaRef {
    /// A named schema with no JSON-schema body.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            json_schema: None,
        }
    }

    /// Attach a JSON-schema body.
    pub fn with_json_schema(mut self, json_schema: serde_json::Value) -> Self {
        self.json_schema = Some(json_schema);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_schema_round_trips_without_body() {
        let schema = SchemaRef::named("Summary");
        let json = serde_json::to_string(&schema).unwrap();
        assert_eq!(json, r#"{"name":"Summary"}"#);
        assert_eq!(serde_json::from_str::<SchemaRef>(&json).unwrap(), schema);
    }
}
