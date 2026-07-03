use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use pocopine_agenkit::server::{Principal, SecretString};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};

use crate::policy::{
    ApprovalDecision, ApprovalRequest, ToolApprover, ToolMode, no_approver_reason,
};

use super::resolver::{AgentSecretResolver, InMemorySecretResolver};
use super::tools::{
    SECRET_REQUEST_TOOL_ID, SECRET_REVOKE_TOOL_ID, SECRET_USE_TOOL_ID, SecretRequestInput,
};
use super::types::{SecretGrant, SecretMetadata};

const DEFAULT_GRANT_TTL_MS: u64 = 15 * 60 * 1000;
const MAX_GRANT_TTL_MS: u64 = 60 * 60 * 1000;

struct GrantRecord {
    owner: Principal,
    grant: SecretGrant,
    revoked: bool,
}

struct SecretRuntimeInner {
    grants: HashMap<String, GrantRecord>,
    counter: u64,
}

/// Outcome of the grant-request policy gate before any approver is consulted.
enum RequestGate {
    Allowed,
    NeedsApproval,
}

/// Exact request tuple that may be pre-authorized by a host policy.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SecretGrantKey {
    pub secret_ref: String,
    pub purpose: String,
    pub target_tool: String,
    pub destination: Option<String>,
}

impl SecretGrantKey {
    pub fn new(
        secret_ref: impl Into<String>,
        purpose: impl Into<String>,
        target_tool: impl Into<String>,
        destination: Option<impl Into<String>>,
    ) -> Self {
        Self {
            secret_ref: secret_ref.into(),
            purpose: purpose.into(),
            target_tool: target_tool.into(),
            destination: destination.map(Into::into),
        }
    }

    fn from_request(input: &SecretRequestInput) -> Self {
        Self {
            secret_ref: input.secret_ref.clone(),
            purpose: input.purpose.clone(),
            target_tool: input.target_tool.clone(),
            destination: input.destination.clone(),
        }
    }
}

/// Request policy for issuing new secret grants. `Ask` and `Deny` fail closed
/// unless the exact request tuple has been pre-authorized by the host.
#[derive(Clone, Debug)]
pub struct SecretRequestPolicy {
    pub mode: ToolMode,
    preauthorized: HashSet<SecretGrantKey>,
}

impl Default for SecretRequestPolicy {
    fn default() -> Self {
        Self {
            mode: ToolMode::Ask,
            preauthorized: HashSet::new(),
        }
    }
}

/// Session/local runtime for secret handles. Cloning it shares the same grant
/// table across `secret.*`, `process.*`, and `net.*` tools registered together.
#[derive(Clone)]
pub struct SecretRuntime {
    resolver: Arc<dyn AgentSecretResolver>,
    policy: SecretRequestPolicy,
    /// Host approver for `Ask`-mode grant requests (M1d). `None` = every
    /// non-preauthorized Ask fails closed.
    approver: Option<Arc<dyn ToolApprover>>,
    inner: Arc<Mutex<SecretRuntimeInner>>,
}

impl std::fmt::Debug for SecretRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretRuntime")
            .field("grants", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl SecretRuntime {
    pub fn new(resolver: Arc<dyn AgentSecretResolver>) -> Self {
        Self {
            resolver,
            policy: SecretRequestPolicy::default(),
            approver: None,
            inner: Arc::new(Mutex::new(SecretRuntimeInner {
                grants: HashMap::new(),
                counter: 0,
            })),
        }
    }

    pub fn empty() -> Self {
        Self::new(Arc::new(InMemorySecretResolver::default()))
    }

    /// Set the grant-request mode. Hosts running non-interactively should use
    /// `Deny` plus explicit pre-authorizations; local/dev embedders may choose
    /// `Allow` when the resolver itself is trusted and scoped.
    pub fn with_request_mode(mut self, mode: ToolMode) -> Self {
        self.policy.mode = mode;
        self
    }

    /// Pre-authorize one exact secret request tuple. This lets non-interactive
    /// runs issue a scoped handle without prompting while other requests fail
    /// closed.
    pub fn with_preauthorized_grant(mut self, grant: SecretGrantKey) -> Self {
        self.policy.preauthorized.insert(grant);
        self
    }

    /// Install the host approver consulted for `Ask`-mode grant requests that
    /// are not pre-authorized. Without one they fail closed.
    pub fn with_approver(mut self, approver: Arc<dyn ToolApprover>) -> Self {
        self.approver = Some(approver);
        self
    }

    pub async fn list(&self, principal: &Principal) -> AgenkitResult<Vec<SecretMetadata>> {
        self.resolver.list(principal).await
    }

    pub async fn request(
        &self,
        input: SecretRequestInput,
        principal: &Principal,
    ) -> AgenkitResult<SecretGrant> {
        if input.secret_ref.trim().is_empty() {
            return Err(AgenkitError::validation("secret_ref is required"));
        }
        if input.purpose.trim().is_empty() {
            return Err(AgenkitError::validation("purpose is required"));
        }
        if input.target_tool.trim().is_empty() {
            return Err(AgenkitError::validation("target_tool is required"));
        }

        let metadata = self.resolver.list(principal).await?;
        let Some(secret) = metadata
            .iter()
            .find(|secret| secret.secret_ref == input.secret_ref)
        else {
            return Err(AgenkitError::not_found(format!(
                "secret `{}` is not available",
                input.secret_ref
            )));
        };
        if !secret.permits(
            &input.purpose,
            &input.target_tool,
            input.destination.as_deref(),
        ) {
            return Err(AgenkitError::tool_policy(format!(
                "secret `{}` is not authorized for `{}`",
                input.secret_ref, input.target_tool
            )));
        }
        match self.request_gate(&input)? {
            RequestGate::Allowed => {}
            RequestGate::NeedsApproval => {
                let reason = format!(
                    "secret request `{}` for `{}` requires host approval",
                    input.secret_ref, input.target_tool
                );
                let Some(approver) = &self.approver else {
                    return Err(AgenkitError::tool_policy(no_approver_reason(&reason)));
                };
                // The detail carries every security-relevant field the operator
                // is authorizing — including the lifetime knobs (`ttl_ms`,
                // `inheritable`) the model chose, which are applied verbatim
                // after approval. Omitting them would let an operator approve a
                // max-TTL or inheritable grant without seeing it.
                let request = ApprovalRequest::new(SECRET_REQUEST_TOOL_ID, reason.clone())
                    .with_detail(serde_json::json!({
                        "secret_ref": input.secret_ref,
                        "purpose": input.purpose,
                        "target_tool": input.target_tool,
                        "destination": input.destination,
                        "ttl_ms": input.ttl_ms,
                        "inheritable": input.inheritable.unwrap_or(false),
                    }));
                match approver.approve(request).await {
                    ApprovalDecision::Approved => {}
                    ApprovalDecision::Denied { reason } => {
                        return Err(AgenkitError::tool_policy(format!(
                            "secret request denied: {reason}"
                        )));
                    }
                }
            }
        }

        let now = current_time_ms();
        let ttl = input
            .ttl_ms
            .unwrap_or(DEFAULT_GRANT_TTL_MS)
            .clamp(1, MAX_GRANT_TTL_MS);
        let grant = {
            let mut inner = self.inner.lock().expect("secret runtime poisoned");
            inner.counter += 1;
            let handle_id = format!("secret-{}-{:016x}", inner.counter, rand::random::<u64>());
            let grant = SecretGrant {
                handle_id: handle_id.clone(),
                secret_ref: input.secret_ref,
                purpose: input.purpose,
                target_tool: input.target_tool,
                destination: input.destination,
                expires_at_ms: Some(now.saturating_add(ttl)),
                inheritable: input.inheritable.unwrap_or(false),
            };
            inner.grants.insert(
                handle_id,
                GrantRecord {
                    owner: principal.clone(),
                    grant: grant.clone(),
                    revoked: false,
                },
            );
            grant
        };

        tracing::info!(
            target: "pocopine.log",
            tool = SECRET_REQUEST_TOOL_ID,
            secret_ref = grant.secret_ref.as_str(),
            target_tool = grant.target_tool.as_str(),
            purpose = grant.purpose.as_str(),
            principal = principal_key(principal),
            destination = ?grant.destination,
            outcome = "issued",
            "secret grant issued"
        );
        Ok(grant)
    }

    fn request_gate(&self, input: &SecretRequestInput) -> AgenkitResult<RequestGate> {
        let key = SecretGrantKey::from_request(input);
        if self.policy.preauthorized.contains(&key) {
            return Ok(RequestGate::Allowed);
        }
        match self.policy.mode {
            ToolMode::Allow => Ok(RequestGate::Allowed),
            ToolMode::Ask => Ok(RequestGate::NeedsApproval),
            ToolMode::Deny => Err(AgenkitError::tool_policy(format!(
                "secret request `{}` for `{}` is not pre-authorized",
                input.secret_ref, input.target_tool
            ))),
        }
    }

    pub fn validate_use(
        &self,
        handle_id: &str,
        principal: &Principal,
        target_tool: &str,
        destination: Option<&str>,
    ) -> AgenkitResult<SecretGrant> {
        let inner = self.inner.lock().expect("secret runtime poisoned");
        let Some(record) = inner.grants.get(handle_id) else {
            audit_use_denied(principal, target_tool, destination, "not_found");
            return Err(AgenkitError::not_found(format!(
                "unknown secret handle `{handle_id}`"
            )));
        };
        if let Err(err) = validate_record(record, principal, target_tool, destination) {
            audit_use(record, principal, destination, "denied");
            return Err(err);
        }
        audit_use(record, principal, destination, "allowed");
        Ok(record.grant.clone())
    }

    pub async fn resolve_handle(
        &self,
        handle_id: &str,
        principal: &Principal,
        target_tool: &str,
        destination: Option<&str>,
    ) -> AgenkitResult<SecretString> {
        let grant = self.validate_use(handle_id, principal, target_tool, destination)?;
        self.resolver
            .resolve(
                &grant.secret_ref,
                principal,
                &grant.purpose,
                target_tool,
                destination,
            )
            .await?
            .ok_or_else(|| {
                AgenkitError::not_found(format!("secret `{}` is unavailable", grant.secret_ref))
            })
    }

    pub fn revoke(&self, handle_id: &str, principal: &Principal) -> AgenkitResult<bool> {
        let mut inner = self.inner.lock().expect("secret runtime poisoned");
        let Some(record) = inner.grants.get_mut(handle_id) else {
            return Err(AgenkitError::not_found(format!(
                "unknown secret handle `{handle_id}`"
            )));
        };
        if &record.owner != principal {
            return Err(AgenkitError::not_found(format!(
                "unknown secret handle `{handle_id}`"
            )));
        }
        let changed = !record.revoked;
        record.revoked = true;
        tracing::info!(
            target: "pocopine.log",
            tool = SECRET_REVOKE_TOOL_ID,
            handle_id,
            secret_ref = record.grant.secret_ref.as_str(),
            "secret grant revoked"
        );
        Ok(changed)
    }

    /// Export inheritable grant metadata for a fork/resume boundary. Values are
    /// never exported, and revoked/expired/non-inheritable grants are skipped.
    pub fn export_inheritable_grants(&self, principal: &Principal) -> Vec<SecretGrant> {
        let inner = self.inner.lock().expect("secret runtime poisoned");
        inner
            .grants
            .values()
            .filter(|record| {
                &record.owner == principal
                    && record.grant.inheritable
                    && !record.revoked
                    && !grant_expired(&record.grant)
            })
            .map(|record| record.grant.clone())
            .collect()
    }

    /// Import inheritable grant metadata into this runtime for `principal`,
    /// minting fresh opaque handle ids. The resolver still controls whether the
    /// value can be resolved at use time.
    pub fn import_inherited_grants(
        &self,
        principal: &Principal,
        grants: impl IntoIterator<Item = SecretGrant>,
    ) -> Vec<SecretGrant> {
        let mut inner = self.inner.lock().expect("secret runtime poisoned");
        let mut imported = Vec::new();
        for grant in grants {
            if !grant.inheritable || grant_expired(&grant) {
                continue;
            }
            inner.counter += 1;
            let handle_id = format!("secret-{}-{:016x}", inner.counter, rand::random::<u64>());
            let mut grant = grant;
            grant.handle_id = handle_id.clone();
            inner.grants.insert(
                handle_id,
                GrantRecord {
                    owner: principal.clone(),
                    grant: grant.clone(),
                    revoked: false,
                },
            );
            imported.push(grant);
        }
        imported
    }
}

fn grant_expired(grant: &SecretGrant) -> bool {
    grant
        .expires_at_ms
        .is_some_and(|expires| expires <= current_time_ms())
}

fn validate_record(
    record: &GrantRecord,
    principal: &Principal,
    target_tool: &str,
    destination: Option<&str>,
) -> AgenkitResult<()> {
    if &record.owner != principal {
        return Err(AgenkitError::not_found(format!(
            "unknown secret handle `{}`",
            record.grant.handle_id
        )));
    }
    if record.revoked {
        return Err(AgenkitError::tool_policy(format!(
            "secret handle `{}` has been revoked",
            record.grant.handle_id
        )));
    }
    if grant_expired(&record.grant) {
        return Err(AgenkitError::tool_policy(format!(
            "secret handle `{}` has expired",
            record.grant.handle_id
        )));
    }
    if record.grant.target_tool != target_tool {
        return Err(AgenkitError::tool_policy(format!(
            "secret handle `{}` is scoped to `{}`",
            record.grant.handle_id, record.grant.target_tool
        )));
    }
    if let Some(expected) = record.grant.destination.as_deref()
        && destination != Some(expected)
    {
        return Err(AgenkitError::tool_policy(format!(
            "secret handle `{}` is scoped to destination `{expected}`",
            record.grant.handle_id
        )));
    }
    Ok(())
}

fn audit_use(
    record: &GrantRecord,
    principal: &Principal,
    destination: Option<&str>,
    outcome: &'static str,
) {
    tracing::info!(
        target: "pocopine.log",
        tool = SECRET_USE_TOOL_ID,
        secret_ref = record.grant.secret_ref.as_str(),
        target_tool = record.grant.target_tool.as_str(),
        purpose = record.grant.purpose.as_str(),
        principal = principal_key(principal),
        destination = destination.or(record.grant.destination.as_deref()),
        outcome,
        "secret grant use checked"
    );
}

fn audit_use_denied(
    principal: &Principal,
    target_tool: &str,
    destination: Option<&str>,
    outcome: &'static str,
) {
    tracing::info!(
        target: "pocopine.log",
        tool = SECRET_USE_TOOL_ID,
        target_tool,
        principal = principal_key(principal),
        destination,
        outcome,
        "secret grant use denied"
    );
}

fn principal_key(principal: &Principal) -> &str {
    principal
        .user()
        .map_or("anonymous", |user| user.id.as_str())
}

pub fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use pocopine_agenkit::server::{AuthUser, Principal, SecretString};

    use super::*;
    use crate::tools::secrets::{
        InMemorySecretResolver, SecretMetadata, SecretRequestInput, SecretScope,
    };

    fn runtime() -> SecretRuntime {
        SecretRuntime::new(Arc::new(
            InMemorySecretResolver::new().insert(
                SecretMetadata::new("github-token", "GitHub token", SecretScope::User)
                    .with_purposes(["repo-read"])
                    .with_target_tools(["process.run"])
                    .with_destinations(["GITHUB_TOKEN"]),
                SecretString::new("ghp_secret"),
            ),
        ))
        .with_request_mode(ToolMode::Allow)
    }

    fn request() -> SecretRequestInput {
        SecretRequestInput {
            secret_ref: "github-token".to_string(),
            purpose: "repo-read".to_string(),
            target_tool: "process.run".to_string(),
            destination: Some("GITHUB_TOKEN".to_string()),
            ttl_ms: Some(60_000),
            inheritable: None,
        }
    }

    #[tokio::test]
    async fn list_and_request_do_not_expose_values() {
        let runtime = runtime();
        let principal = Principal::anonymous();
        let list = runtime.list(&principal).await.unwrap();
        let json = serde_json::to_string(&list).unwrap();
        assert!(json.contains("github-token"));
        assert!(!json.contains("ghp_secret"));

        let grant = runtime.request(request(), &principal).await.unwrap();
        let json = serde_json::to_string(&grant).unwrap();
        assert!(json.contains("github-token"));
        assert!(!json.contains("ghp_secret"));
        assert!(grant.handle_id.starts_with("secret-"));
    }

    #[tokio::test]
    async fn handle_resolves_for_owner_tool_and_destination() {
        let runtime = runtime();
        let principal = Principal::anonymous();
        let grant = runtime.request(request(), &principal).await.unwrap();
        let secret = runtime
            .resolve_handle(
                &grant.handle_id,
                &principal,
                "process.run",
                Some("GITHUB_TOKEN"),
            )
            .await
            .unwrap();
        assert_eq!(secret.expose(), "ghp_secret");
    }

    #[tokio::test]
    async fn wrong_owner_tool_destination_and_revocation_fail_closed() {
        let runtime = runtime();
        let alice = Principal::from_user(AuthUser::new("alice"));
        let bob = Principal::from_user(AuthUser::new("bob"));
        let grant = runtime.request(request(), &alice).await.unwrap();

        assert_eq!(
            runtime
                .resolve_handle(&grant.handle_id, &bob, "process.run", Some("GITHUB_TOKEN"))
                .await
                .unwrap_err()
                .kind(),
            "not_found"
        );
        assert_eq!(
            runtime
                .resolve_handle(&grant.handle_id, &alice, "net.fetch", Some("GITHUB_TOKEN"))
                .await
                .unwrap_err()
                .kind(),
            "tool_policy"
        );
        assert_eq!(
            runtime
                .resolve_handle(&grant.handle_id, &alice, "process.run", Some("OTHER"))
                .await
                .unwrap_err()
                .kind(),
            "tool_policy"
        );
        assert!(runtime.revoke(&grant.handle_id, &alice).unwrap());
        assert_eq!(
            runtime
                .resolve_handle(
                    &grant.handle_id,
                    &alice,
                    "process.run",
                    Some("GITHUB_TOKEN")
                )
                .await
                .unwrap_err()
                .kind(),
            "tool_policy"
        );
    }

    #[tokio::test]
    async fn default_request_policy_requires_approval() {
        let runtime = SecretRuntime::new(Arc::new(InMemorySecretResolver::new().insert(
            SecretMetadata::new("github-token", "GitHub token", SecretScope::User),
            SecretString::new("ghp_secret"),
        )));

        let err = runtime
            .request(request(), &Principal::anonymous())
            .await
            .unwrap_err();

        assert_eq!(err.kind(), "tool_policy");
        assert!(err.to_string().contains("requires host approval"));
    }

    #[tokio::test]
    async fn approver_approval_issues_an_ask_mode_grant() {
        let runtime = SecretRuntime::new(Arc::new(InMemorySecretResolver::new().insert(
            SecretMetadata::new("github-token", "GitHub token", SecretScope::User),
            SecretString::new("ghp_secret"),
        )))
        .with_approver(Arc::new(crate::policy::StaticApprover(true)));

        let grant = runtime
            .request(request(), &Principal::anonymous())
            .await
            .unwrap();
        assert_eq!(grant.secret_ref, "github-token");
    }

    #[tokio::test]
    async fn approval_detail_carries_every_security_relevant_field() {
        use crate::policy::{ApprovalDecision, ApprovalRequest, ToolApprover};
        use pocopine_agenkit::server::BoxFuture;

        // A recording approver captures the detail the operator would see.
        struct Recorder(std::sync::Mutex<Option<ApprovalRequest>>);
        impl ToolApprover for Recorder {
            fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
                *self.0.lock().unwrap() = Some(request);
                Box::pin(async { ApprovalDecision::Approved })
            }
        }
        let recorder = Arc::new(Recorder(std::sync::Mutex::new(None)));
        let runtime = SecretRuntime::new(Arc::new(
            InMemorySecretResolver::new().insert(
                SecretMetadata::new("github-token", "GitHub token", SecretScope::User)
                    .with_purposes(["repo-read"])
                    .with_target_tools(["process.run"])
                    .with_destinations(["GITHUB_TOKEN"]),
                SecretString::new("ghp_secret"),
            ),
        ))
        .with_approver(recorder.clone());

        let input = SecretRequestInput {
            ttl_ms: Some(3_600_000),
            inheritable: Some(true),
            ..request()
        };
        runtime
            .request(input, &Principal::anonymous())
            .await
            .unwrap();

        let seen = recorder.0.lock().unwrap().clone().expect("approver ran");
        let detail = seen.detail.expect("detail present");
        // The lifetime knobs the model chose must be visible to the operator.
        assert_eq!(detail["ttl_ms"], 3_600_000);
        assert_eq!(detail["inheritable"], true);
        assert_eq!(detail["target_tool"], "process.run");
        assert_eq!(detail["destination"], "GITHUB_TOKEN");
    }

    #[tokio::test]
    async fn approver_denial_fails_an_ask_mode_grant() {
        let runtime = SecretRuntime::new(Arc::new(InMemorySecretResolver::new().insert(
            SecretMetadata::new("github-token", "GitHub token", SecretScope::User),
            SecretString::new("ghp_secret"),
        )))
        .with_approver(Arc::new(crate::policy::StaticApprover(false)));

        let err = runtime
            .request(request(), &Principal::anonymous())
            .await
            .unwrap_err();
        assert_eq!(err.kind(), "tool_policy");
        assert!(err.to_string().contains("denied"));
    }

    #[tokio::test]
    async fn preauthorized_request_can_issue_grant_when_mode_denies() {
        let runtime = SecretRuntime::new(Arc::new(
            InMemorySecretResolver::new().insert(
                SecretMetadata::new("github-token", "GitHub token", SecretScope::User)
                    .with_purposes(["repo-read"])
                    .with_target_tools(["process.run"])
                    .with_destinations(["GITHUB_TOKEN"]),
                SecretString::new("ghp_secret"),
            ),
        ))
        .with_request_mode(ToolMode::Deny)
        .with_preauthorized_grant(SecretGrantKey::new(
            "github-token",
            "repo-read",
            "process.run",
            Some("GITHUB_TOKEN"),
        ));

        let grant = runtime
            .request(request(), &Principal::anonymous())
            .await
            .unwrap();

        assert_eq!(grant.secret_ref, "github-token");
    }

    #[tokio::test]
    async fn inheritance_exports_metadata_only_and_mints_new_handles() {
        let parent = runtime();
        let child = runtime();
        let principal = Principal::anonymous();
        let mut req = request();
        req.inheritable = Some(true);
        let grant = parent.request(req, &principal).await.unwrap();

        let exported = parent.export_inheritable_grants(&principal);
        let imported = child.import_inherited_grants(&principal, exported);

        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].secret_ref, grant.secret_ref);
        assert_ne!(imported[0].handle_id, grant.handle_id);
        let secret = child
            .resolve_handle(
                &imported[0].handle_id,
                &principal,
                "process.run",
                Some("GITHUB_TOKEN"),
            )
            .await
            .unwrap();
        assert_eq!(secret.expose(), "ghp_secret");
    }
}
