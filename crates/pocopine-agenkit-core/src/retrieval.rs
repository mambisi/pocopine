//! Retrieval / RAG payloads (RFC-093 Phase 1.3, §D5).
//!
//! A retriever is a read-oriented context source. Descriptors declare source
//! kinds and auth requirements; hits carry citations and source refs so
//! reducers and traces can cite evidence without storing raw document bodies
//! by default (§D5, §D12).

use crate::{Content, SchemaRef};

/// A reference to a retrieval source (a document, trace, log, row, ...).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SourceRef {
    /// Stable id of the source within its kind.
    pub id: String,
    /// The source kind (e.g. `"doc"`, `"trace"`, `"build_log"`).
    pub kind: String,
    /// Optional URI/locator for the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl SourceRef {
    /// A source ref of the given kind.
    pub fn new(kind: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            uri: None,
        }
    }

    /// Attach a URI/locator.
    pub fn with_uri(mut self, uri: impl Into<String>) -> Self {
        self.uri = Some(uri.into());
        self
    }
}

/// A citation backing a claim, pointing at a [`SourceRef`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Citation {
    /// The cited source.
    pub source: SourceRef,
    /// An optional supporting snippet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

/// The framework-visible description of a registered retriever.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetrieverDescriptor {
    /// Stable retriever id.
    pub id: String,
    /// What the retriever searches.
    pub description: String,
    /// Schema of the retriever's typed query. Carries a JSON-schema body only
    /// when the host runtime derives it (e.g. for the retriever-as-tool
    /// adapter's `parameters`); metadata only, never a value or credential.
    #[serde(default)]
    pub query: SchemaRef,
    /// The source kinds it can return.
    #[serde(default)]
    pub source_kinds: Vec<String>,
    /// Whether retrieval requires an authenticated principal.
    #[serde(default)]
    pub requires_auth: bool,
}

impl RetrieverDescriptor {
    /// A retriever descriptor.
    pub fn new(id: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            description: description.into(),
            query: SchemaRef::default(),
            source_kinds: Vec::new(),
            requires_auth: false,
        }
    }

    /// Set the query schema.
    pub fn with_query(mut self, query: SchemaRef) -> Self {
        self.query = query;
        self
    }

    /// Declare the source kinds returned.
    pub fn with_source_kinds<I, S>(mut self, kinds: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.source_kinds = kinds.into_iter().map(Into::into).collect();
        self
    }

    /// Require an authenticated principal for retrieval.
    pub fn requires_auth(mut self) -> Self {
        self.requires_auth = true;
        self
    }
}

/// A retrieval request.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetrievalQuery {
    /// Free-text query.
    pub text: String,
    /// How many hits to return at most.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Opaque filter object (tenant, kind, time range, ...).
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub filters: serde_json::Value,
}

impl RetrievalQuery {
    /// A query with just free text.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            top_k: None,
            filters: serde_json::Value::Null,
        }
    }

    /// Limit the number of hits.
    pub fn top_k(mut self, k: u32) -> Self {
        self.top_k = Some(k);
        self
    }

    /// Attach a filter object.
    pub fn with_filters(mut self, filters: serde_json::Value) -> Self {
        self.filters = filters;
        self
    }
}

/// One retrieved hit with its score and provenance.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetrievalHit {
    /// Where the hit came from.
    pub source: SourceRef,
    /// Relevance score, higher is better (semantics are retriever-defined).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    /// The retrieved content.
    pub content: Content,
    /// An optional citation derived from the hit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation: Option<Citation>,
}

/// An ordered set of retrieval hits.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct RetrievalSet {
    /// The hits, best-first by convention.
    pub hits: Vec<RetrievalHit>,
}

impl RetrievalSet {
    /// Build a set from hits.
    pub fn new(hits: Vec<RetrievalHit>) -> Self {
        Self { hits }
    }

    /// Number of hits.
    pub fn len(&self) -> usize {
        self.hits.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    /// The source refs of every hit, for trace/citation metadata.
    pub fn source_refs(&self) -> Vec<&SourceRef> {
        self.hits.iter().map(|hit| &hit.source).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_omits_defaults_and_round_trips() {
        let query = RetrievalQuery::new("how do uploads work?").top_k(5);
        let json = serde_json::to_string(&query).unwrap();
        assert!(!json.contains("filters"));
        assert!(json.contains(r#""top_k":5"#));
        assert_eq!(
            serde_json::from_str::<RetrievalQuery>(&json).unwrap(),
            query
        );
    }

    #[test]
    fn set_reports_sources() {
        let set = RetrievalSet::new(vec![RetrievalHit {
            source: SourceRef::new("doc", "uploads.md").with_uri("docs/uploads.md"),
            score: Some(0.91),
            content: Content::text("uploads use presigned URLs"),
            citation: None,
        }]);
        assert_eq!(set.len(), 1);
        assert_eq!(set.source_refs()[0].id, "uploads.md");
        let back: RetrievalSet =
            serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
        assert_eq!(set, back);
    }
}
