//! Invariant: client-safe contract payloads never carry credential-shaped
//! fields (RFC-093 §D10). This is the seed of the Phase 3 secret-boundary
//! suite — it runs against the descriptor/payload types as they are added.

use pocopine_agenkit_core::{
    Content, EmbedderDescriptor, ModelRef, RetrievalHit, RetrievalQuery, RetrievalSet,
    RetrieverDescriptor, SchemaRef, SourceRef, ToolCall, ToolDescriptor, ToolResult,
};

/// Substrings that must never appear as field names in a serialized payload.
const FORBIDDEN: &[&str] = &[
    "api_key",
    "apikey",
    "authorization",
    "secret",
    "bearer",
    "password",
    "access_token",
    "refresh_token",
    "private_key",
    "credential",
];

fn assert_no_secret_fields(label: &str, json: &str) {
    let lower = json.to_lowercase();
    for needle in FORBIDDEN {
        assert!(
            !lower.contains(needle),
            "{label} serialized payload contains forbidden substring {needle:?}: {json}"
        );
    }
}

#[test]
fn capability_descriptors_have_no_credential_fields() {
    let tool = ToolDescriptor::new("search_docs", "Search project docs")
        .with_input(SchemaRef::named("SearchDocs"))
        .with_output(SchemaRef::named("Vec<SearchHit>"))
        .side_effecting();
    assert_no_secret_fields("ToolDescriptor", &serde_json::to_string(&tool).unwrap());

    let retriever = RetrieverDescriptor::new("project_docs", "Search docs")
        .with_source_kinds(["doc"])
        .requires_auth();
    assert_no_secret_fields(
        "RetrieverDescriptor",
        &serde_json::to_string(&retriever).unwrap(),
    );

    let embedder = EmbedderDescriptor::new("local", ModelRef::new("local/embed"), 384);
    assert_no_secret_fields(
        "EmbedderDescriptor",
        &serde_json::to_string(&embedder).unwrap(),
    );
}

#[test]
fn call_result_and_hits_have_no_credential_fields() {
    let call = ToolCall::new("c1", "search_docs", serde_json::json!({"query": "uploads"}));
    assert_no_secret_fields("ToolCall", &serde_json::to_string(&call).unwrap());

    let result = ToolResult::ok("c1", serde_json::json!([{"title": "Uploads"}]));
    assert_no_secret_fields("ToolResult", &serde_json::to_string(&result).unwrap());

    let query = RetrievalQuery::new("uploads").top_k(5);
    assert_no_secret_fields("RetrievalQuery", &serde_json::to_string(&query).unwrap());

    let set = RetrievalSet::new(vec![RetrievalHit {
        source: SourceRef::new("doc", "uploads.md"),
        score: Some(0.9),
        content: Content::text("uploads use presigned URLs"),
        citation: None,
    }]);
    assert_no_secret_fields("RetrievalSet", &serde_json::to_string(&set).unwrap());
}
