//! `net.download` — fetch one allowlisted URL (GET) and store the bounded body
//! as a content-addressed **artifact** instead of returning it to the model.
//!
//! The network path (policy + SSRF guard → pinned per-hop redirect follow →
//! capped body) is the shared [`GuardedHttp`](super::http) engine — identical
//! to `net.fetch`. This tool differs only in the terminus: rather than
//! rendering markdown, it writes the raw bytes to the [`ArtifactStore`] and
//! returns the artifact id + metadata (name, media type, size, SHA-256). Binary
//! is expected here, so there is no content-type gate.

use std::collections::BTreeMap;
use std::sync::Arc;

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture, Principal};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::http::GuardedHttp;
use super::policy::NetPolicy;
use super::ssrf::Resolve;
use crate::tools::artifacts::{
    ArtifactDraft, ArtifactRuntime, ArtifactScope, MAX_CONTENT_BYTES, current_time_ms,
    reject_secret_like_content, validate_artifact_name, validate_media_type,
};
use crate::tools::secrets::SecretRuntime;

pub const NET_DOWNLOAD_TOOL_ID: &str = "net.download";

/// The `net.download` tool: the shared guarded-HTTP engine plus the artifact
/// runtime it stores into. The artifact runtime handle is constructor-injected
/// (like the `artifact.*` tools); the caller identity/namespace arrives per
/// call as a runtime-injected `context_token`.
pub struct NetDownloadTool {
    http: GuardedHttp,
    artifacts: Arc<ArtifactRuntime>,
}

impl NetDownloadTool {
    /// Build with the production hickory resolver.
    pub fn new(policy: NetPolicy, artifacts: Arc<ArtifactRuntime>) -> super::NetResult<Self> {
        Ok(Self {
            http: GuardedHttp::new(policy)?,
            artifacts,
        })
    }

    /// Build with an injected resolver (tests).
    pub fn with_resolver(
        policy: NetPolicy,
        resolver: Arc<dyn Resolve>,
        artifacts: Arc<ArtifactRuntime>,
    ) -> Self {
        Self {
            http: GuardedHttp::with_resolver(policy, resolver),
            artifacts,
        }
    }

    pub fn with_secret_runtime(mut self, runtime: Arc<SecretRuntime>) -> Self {
        self.http.set_secret_runtime(runtime);
        self
    }

    async fn download(
        &self,
        input: NetDownloadInput,
        principal: &Principal,
    ) -> AgenkitResult<NetDownloadOutput> {
        // Resolve the caller's artifact context (namespace) from the
        // runtime-injected token — never from the model.
        let context = self
            .artifacts
            .take_context(input.context_token.as_deref().unwrap_or(""))?;
        let scope = input.scope.unwrap_or(ArtifactScope::Session);
        let namespace = context.namespace_for(scope).ok_or_else(|| {
            AgenkitError::tool_policy(format!(
                "artifact scope `{}` is not available in this run",
                scope.as_str()
            ))
        })?;
        let name = validate_artifact_name(&input.name)?;

        // Clamp the download to the artifact cap (below the net policy's
        // response cap) so the body is never larger than the store accepts.
        let cap = self
            .http
            .policy()
            .max_response_bytes()
            .min(MAX_CONTENT_BYTES);
        let response = self
            .http
            .get(
                &input.url,
                &input.secret_headers,
                principal,
                NET_DOWNLOAD_TOOL_ID,
                cap,
            )
            .await?;
        if response.truncated {
            return Err(AgenkitError::validation(format!(
                "download exceeded the {MAX_CONTENT_BYTES} byte artifact cap"
            )));
        }
        // Same durable-artifact contract as artifact.write/link: a body that
        // looks like credential material is refused — net.download must not be
        // a side door for persisting secrets.
        reject_secret_like_content(&response.body)?;
        // The declared media type is advisory; validate/normalize it, falling
        // back to a generic binary type when the caller gave none and the
        // server sent none.
        let media_type = validate_media_type(Some(
            input
                .media_type
                .or_else(|| non_empty(&response.content_type))
                .unwrap_or_else(|| "application/octet-stream".to_string()),
        ))?;

        // Same scope policy as `artifact.write`: session scope proceeds,
        // project scope consults the host approver (fails closed headless).
        self.artifacts
            .authorize_scope_write(
                NET_DOWNLOAD_TOOL_ID,
                scope,
                serde_json::json!({
                    "name": name,
                    "media_type": media_type,
                    "size": response.body.len(),
                    "scope": scope.as_str(),
                }),
            )
            .await?;

        let stored = self
            .artifacts
            .store()
            .write(
                ArtifactDraft {
                    name,
                    media_type,
                    scope,
                    namespace,
                    // Record the originating session on the artifact.
                    source_refs: context.thread_ref().into_iter().collect(),
                    link_path: None,
                    created_at_ms: current_time_ms(),
                },
                response.body,
            )
            .await?;

        // RFC-069 `pocopine.log`: outcome only — never the full URL (queries
        // can carry secrets), headers, or bytes.
        tracing::info!(
            target: "pocopine.log",
            tool = NET_DOWNLOAD_TOOL_ID,
            status = response.status.as_u16(),
            artifact_id = %stored.id,
            size = stored.size,
            "net.download stored artifact"
        );

        Ok(NetDownloadOutput {
            artifact_id: stored.id,
            name: stored.name,
            media_type: stored.media_type,
            size: stored.size,
            sha256: stored.sha256,
            status: response.status.as_u16(),
            final_url: response.final_url,
        })
    }
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.split(';').next().unwrap_or("").trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

impl AiTool for NetDownloadTool {
    const ID: &'static str = NET_DOWNLOAD_TOOL_ID;
    type Input = NetDownloadInput;
    type Output = NetDownloadOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            NET_DOWNLOAD_TOOL_ID,
            "Download one allowlisted URL with GET and store its bounded body as a durable, \
             content-hashed artifact (returns the artifact id, not the bytes). Private, \
             metadata, and internal network targets are always blocked. Use artifact.read to \
             inspect the result.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let principal = ctx.principal().clone();
        Box::pin(async move { self.download(input, &principal).await })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct NetDownloadInput {
    /// The URL to download. Must be allowlisted and pass the SSRF guard; https
    /// only by default.
    pub url: String,
    /// Artifact name for the stored file (a single path component).
    pub name: String,
    /// Declared media type. Defaults to the server's `Content-Type`, then
    /// `application/octet-stream`.
    #[serde(default)]
    pub media_type: Option<String>,
    /// Artifact scope. `session` (default) or `project` (requires host
    /// approval).
    #[serde(default)]
    pub scope: Option<ArtifactScope>,
    /// Map header name -> approved secret handle, sent only to the origin host.
    #[serde(default)]
    pub secret_headers: BTreeMap<String, String>,
    #[serde(default)]
    pub context_token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
pub struct NetDownloadOutput {
    /// The stored artifact id (`art-…`) — cite this, then `artifact.read` it.
    pub artifact_id: String,
    pub name: String,
    pub media_type: String,
    pub size: u64,
    pub sha256: String,
    pub status: u16,
    pub final_url: String,
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use pocopine_agenkit::server::BoxFuture;

    use super::super::error::NetResult;
    use super::*;
    use crate::tools::artifacts::CurrentArtifactContext;

    struct MockResolver(Vec<IpAddr>);
    impl Resolve for MockResolver {
        fn lookup(&self, _host: &str) -> BoxFuture<'_, NetResult<Vec<IpAddr>>> {
            let addrs = self.0.clone();
            Box::pin(async move { Ok(addrs) })
        }
    }

    fn tool(resolved: &[&str]) -> (NetDownloadTool, Arc<ArtifactRuntime>) {
        let artifacts = Arc::new(ArtifactRuntime::in_memory());
        let addrs = resolved.iter().map(|a| a.parse().unwrap()).collect();
        let tool = NetDownloadTool::with_resolver(
            NetPolicy::allow(["example.com"]),
            Arc::new(MockResolver(addrs)),
            artifacts.clone(),
        );
        (tool, artifacts)
    }

    fn input(runtime: &ArtifactRuntime, body: serde_json::Value) -> NetDownloadInput {
        let context = CurrentArtifactContext {
            project_id: "proj".to_string(),
            thread_id: Some("thread-1".to_string()),
        };
        let args = runtime.inject_context_args(&body, context).unwrap();
        serde_json::from_value(args).unwrap()
    }

    #[tokio::test]
    async fn download_refuses_a_non_allowlisted_host_before_any_write() {
        let (tool, artifacts) = tool(&["93.184.216.34"]);
        let err = tool
            .download(
                input(
                    &artifacts,
                    serde_json::json!({ "url": "https://evil.test/x", "name": "x.bin" }),
                ),
                &Principal::anonymous(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        // Nothing was stored.
        assert!(
            artifacts
                .store()
                .list(
                    &[(ArtifactScope::Session, "thread-1".to_string())],
                    None,
                    10
                )
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn download_refuses_an_ssrf_target_even_when_allowlisted() {
        let (tool, artifacts) = tool(&["169.254.169.254"]);
        let err = tool
            .download(
                input(
                    &artifacts,
                    serde_json::json!({ "url": "https://example.com/x", "name": "x.bin" }),
                ),
                &Principal::anonymous(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
    }

    #[tokio::test]
    async fn download_rejects_a_bad_artifact_name_before_fetching() {
        let (tool, artifacts) = tool(&["93.184.216.34"]);
        let err = tool
            .download(
                input(
                    &artifacts,
                    serde_json::json!({ "url": "https://example.com/x", "name": "../escape" }),
                ),
                &Principal::anonymous(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }

    #[tokio::test]
    async fn download_needs_an_artifact_context() {
        let (tool, _artifacts) = tool(&["93.184.216.34"]);
        // No injected context_token.
        let err = tool
            .download(
                NetDownloadInput {
                    url: "https://example.com/x".to_string(),
                    name: "x.bin".to_string(),
                    media_type: None,
                    scope: None,
                    secret_headers: BTreeMap::new(),
                    context_token: None,
                },
                &Principal::anonymous(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "validation");
    }
}
