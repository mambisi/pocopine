//! Schema derivation for the descriptor layer (RFC-093 §D5).
//!
//! One place that turns a Rust type into a JSON Schema (and a [`SchemaRef`])
//! via [`schemars`]. Structured output, tool input/output, flow input/output,
//! and agent output all route through here, so the model and generated clients
//! see real, consistent schemas — while `pocopine-agenkit-core` stays
//! `schemars`-free and wasm-friendly (DC-1).

use pocopine_agenkit_core::SchemaRef;
use schemars::JsonSchema;

/// The JSON Schema for `T`, derived via `schemars`. Providers that support
/// schema-constrained output use it directly; others fall back to JSON mode,
/// and it doubles as the `parameters` schema for function-calling tools.
pub(crate) fn json_schema_for<T: JsonSchema>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(T))
        .unwrap_or_else(|_| serde_json::json!({ "type": "object" }))
}

/// A [`SchemaRef`] for `T`: the schemars `title` (falling back to the short
/// Rust type name) plus the derived JSON Schema body. Used to fill descriptor
/// input/output schemas at registration without burdening authors.
pub(crate) fn schema_ref_for<T: JsonSchema>() -> SchemaRef {
    let json = json_schema_for::<T>();
    let name = json
        .get("title")
        .and_then(|title| title.as_str())
        .map(str::to_string)
        .unwrap_or_else(short_type_name::<T>);
    SchemaRef {
        name,
        json_schema: Some(json),
    }
}

/// A display name from `std::any::type_name` with module paths stripped but the
/// generic structure kept — a stable-enough name when a type carries no schemars
/// `title` (a container like `Vec<_>`). Keeping the generic args distinguishes
/// `Vec<A>` from `Vec<B>`, which a bare last-segment ("Vec") would collide.
fn short_type_name<T>() -> String {
    let full = std::any::type_name::<T>();
    let mut out = String::with_capacity(full.len());
    let mut segment = String::new();
    for ch in full.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == ':' {
            segment.push(ch);
        } else {
            // End of a `a::b::C` path run — keep only its last segment, then the
            // structural char (`<`, `>`, `,`, space, ...) verbatim.
            out.push_str(segment.rsplit("::").next().unwrap_or(&segment));
            segment.clear();
            out.push(ch);
        }
    }
    out.push_str(segment.rsplit("::").next().unwrap_or(&segment));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, JsonSchema)]
    struct SearchInput {
        query: String,
    }

    #[test]
    fn schema_ref_carries_title_and_body() {
        let schema = schema_ref_for::<SearchInput>();
        assert_eq!(schema.name, "SearchInput");
        let body = schema.json_schema.unwrap();
        assert_eq!(body["properties"]["query"]["type"], "string");
    }

    #[test]
    fn untitled_types_fall_back_to_the_short_type_name() {
        // schemars titles named structs (and even primitives — `u32` -> "uint32"),
        // so the fallback only fires for anonymous shapes like containers. The
        // generic args are kept so distinct containers get distinct names.
        assert_eq!(short_type_name::<Vec<String>>(), "Vec<String>");
        assert_ne!(
            short_type_name::<Vec<String>>(),
            short_type_name::<Vec<u32>>(),
            "distinct container types must not collide to one name"
        );

        // A primitive still yields a real name + body (its schemars title).
        let schema = schema_ref_for::<u32>();
        assert!(!schema.name.is_empty());
        assert!(schema.json_schema.is_some());
    }
}
