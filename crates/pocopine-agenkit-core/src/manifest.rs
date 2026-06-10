//! Flow context manifests and gap diagnostics (RFC-093 Phase 1.4, §D6).
//!
//! A [`ContextManifest`] documents the framework-mediated resources a flow
//! declares it will use. It is documentation and trace metadata, not an
//! enforcement policy: the facade compares actual framework-mediated access
//! against the manifest and emits advisory [`ContextGapDiagnostic`]s.
//!
//! Core does not hash the manifest itself (that would pull a crypto
//! dependency). It exposes [`ContextManifest::canonical_string`]; the facade
//! hashes that via `pocopine-crypto` and stamps the result back as `version`.

use crate::ResourceKind;

/// One declared framework-mediated resource.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeclaredResource {
    /// The resource category.
    pub kind: ResourceKind,
    /// The resource id (tool id, retriever id, state key, model alias, ...).
    pub id: String,
}

impl DeclaredResource {
    /// A declared resource.
    pub fn new(kind: ResourceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

/// The set of resources a flow declares it uses.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextManifest {
    /// The declared resources.
    #[serde(default)]
    pub resources: Vec<DeclaredResource>,
    /// A stable version/hash stamped by the facade (§D6). `None` until set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl ContextManifest {
    /// An empty manifest.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a resource (idempotent: duplicates are ignored).
    pub fn declare(&mut self, kind: ResourceKind, id: impl Into<String>) {
        let resource = DeclaredResource::new(kind, id);
        if !self.resources.contains(&resource) {
            self.resources.push(resource);
        }
    }

    /// Whether a resource of the given kind and id was declared.
    pub fn contains(&self, kind: ResourceKind, id: &str) -> bool {
        self.resources.iter().any(|r| r.kind == kind && r.id == id)
    }

    /// A deterministic, order-independent string the facade hashes to produce
    /// [`ContextManifest::version`]. Sorted so the hash is stable regardless
    /// of declaration order.
    pub fn canonical_string(&self) -> String {
        let mut lines: Vec<String> = self
            .resources
            .iter()
            .map(|r| {
                format!(
                    "{}:{}",
                    serde_json::to_string(&r.kind).unwrap_or_default(),
                    r.id
                )
            })
            .collect();
        lines.sort();
        lines.join("\n")
    }

    /// Set the facade-computed version/hash.
    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = Some(version.into());
        self
    }
}

/// An advisory diagnostic: a framework-mediated resource was accessed but not
/// declared in the flow's manifest (§D6). Emitted to `pocopine.trace`, never
/// an error.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextGapDiagnostic {
    /// The flow that accessed the undeclared resource.
    pub flow_id: String,
    /// The undeclared resource's category.
    pub resource_kind: ResourceKind,
    /// The undeclared resource's id.
    pub resource_id: String,
    /// A human-readable explanation.
    pub message: String,
}

impl ContextGapDiagnostic {
    /// Build a gap diagnostic.
    pub fn new(
        flow_id: impl Into<String>,
        resource_kind: ResourceKind,
        resource_id: impl Into<String>,
    ) -> Self {
        let resource_id = resource_id.into();
        let flow_id = flow_id.into();
        let message = format!(
            "flow `{flow_id}` used an undeclared {resource_kind:?} `{resource_id}`; \
             add it to the flow's context manifest for reliable replay"
        );
        Self {
            flow_id,
            resource_kind,
            resource_id,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declare_is_idempotent_and_queryable() {
        let mut manifest = ContextManifest::new();
        manifest.declare(ResourceKind::Tool, "search_docs");
        manifest.declare(ResourceKind::Tool, "search_docs");
        manifest.declare(ResourceKind::Retriever, "project_docs");
        assert_eq!(manifest.resources.len(), 2);
        assert!(manifest.contains(ResourceKind::Tool, "search_docs"));
        assert!(!manifest.contains(ResourceKind::Tool, "project_docs"));
    }

    #[test]
    fn canonical_string_is_order_independent() {
        let mut a = ContextManifest::new();
        a.declare(ResourceKind::Tool, "t");
        a.declare(ResourceKind::Retriever, "r");
        let mut b = ContextManifest::new();
        b.declare(ResourceKind::Retriever, "r");
        b.declare(ResourceKind::Tool, "t");
        assert_eq!(a.canonical_string(), b.canonical_string());
    }
}
