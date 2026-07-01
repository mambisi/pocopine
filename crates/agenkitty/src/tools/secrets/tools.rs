use std::sync::Arc;

use pocopine_agenkit::server::{AgenkitBuilder, AiTool, AiToolContext, BoxFuture, Principal};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::runtime::SecretRuntime;
use super::types::{SecretGrant, SecretMetadata};

pub const SECRET_LIST_TOOL_ID: &str = "secret.list";
pub const SECRET_REQUEST_TOOL_ID: &str = "secret.request";
pub const SECRET_USE_TOOL_ID: &str = "secret.use";
pub const SECRET_REVOKE_TOOL_ID: &str = "secret.revoke";

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct SecretListInput {
    #[serde(default)]
    pub target_tool: Option<String>,
    #[serde(default)]
    pub purpose: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SecretListOutput {
    pub secrets: Vec<SecretMetadata>,
}

#[derive(Clone)]
pub struct SecretListTool {
    runtime: Arc<SecretRuntime>,
}

impl SecretListTool {
    pub fn new(runtime: Arc<SecretRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(
        &self,
        input: SecretListInput,
        principal: &Principal,
    ) -> AgenkitResult<SecretListOutput> {
        let mut secrets = self.runtime.list(principal).await?;
        if let Some(target) = input.target_tool.as_deref() {
            secrets.retain(|secret| {
                secret.target_tools.is_empty()
                    || secret.target_tools.iter().any(|value| value == target)
            });
        }
        if let Some(purpose) = input.purpose.as_deref() {
            secrets.retain(|secret| {
                secret.purposes.is_empty() || secret.purposes.iter().any(|value| value == purpose)
            });
        }
        Ok(SecretListOutput { secrets })
    }
}

impl AiTool for SecretListTool {
    const ID: &'static str = SECRET_LIST_TOOL_ID;
    type Input = SecretListInput;
    type Output = SecretListOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SECRET_LIST_TOOL_ID,
            "List available secret metadata without exposing raw values.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let principal = ctx.principal().clone();
        Box::pin(async move { self.run(input, &principal).await })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct SecretRequestInput {
    pub secret_ref: String,
    pub purpose: String,
    pub target_tool: String,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub ttl_ms: Option<u64>,
    #[serde(default)]
    pub inheritable: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SecretRequestOutput {
    pub grant: SecretGrant,
}

#[derive(Clone)]
pub struct SecretRequestTool {
    runtime: Arc<SecretRuntime>,
}

impl SecretRequestTool {
    pub fn new(runtime: Arc<SecretRuntime>) -> Self {
        Self { runtime }
    }

    pub async fn run(
        &self,
        input: SecretRequestInput,
        principal: &Principal,
    ) -> AgenkitResult<SecretRequestOutput> {
        Ok(SecretRequestOutput {
            grant: self.runtime.request(input, principal).await?,
        })
    }
}

impl AiTool for SecretRequestTool {
    const ID: &'static str = SECRET_REQUEST_TOOL_ID;
    type Input = SecretRequestInput;
    type Output = SecretRequestOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SECRET_REQUEST_TOOL_ID,
            "Request an opaque, scoped, short-lived secret handle for a declared tool purpose.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let principal = ctx.principal().clone();
        Box::pin(async move { self.run(input, &principal).await })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct SecretUseInput {
    pub handle_id: String,
    pub target_tool: String,
    #[serde(default)]
    pub destination: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SecretUseOutput {
    pub grant: SecretGrant,
}

#[derive(Clone)]
pub struct SecretUseTool {
    runtime: Arc<SecretRuntime>,
}

impl SecretUseTool {
    pub fn new(runtime: Arc<SecretRuntime>) -> Self {
        Self { runtime }
    }

    pub fn run(
        &self,
        input: SecretUseInput,
        principal: &Principal,
    ) -> AgenkitResult<SecretUseOutput> {
        Ok(SecretUseOutput {
            grant: self.runtime.validate_use(
                &input.handle_id,
                principal,
                &input.target_tool,
                input.destination.as_deref(),
            )?,
        })
    }
}

impl AiTool for SecretUseTool {
    const ID: &'static str = SECRET_USE_TOOL_ID;
    type Input = SecretUseInput;
    type Output = SecretUseOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SECRET_USE_TOOL_ID,
            "Validate that a secret handle may be attached to a target tool call.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let principal = ctx.principal().clone();
        Box::pin(async move { self.run(input, &principal) })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct SecretRevokeInput {
    pub handle_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SecretRevokeOutput {
    pub handle_id: String,
    pub revoked: bool,
}

#[derive(Clone)]
pub struct SecretRevokeTool {
    runtime: Arc<SecretRuntime>,
}

impl SecretRevokeTool {
    pub fn new(runtime: Arc<SecretRuntime>) -> Self {
        Self { runtime }
    }

    pub fn run(
        &self,
        input: SecretRevokeInput,
        principal: &Principal,
    ) -> AgenkitResult<SecretRevokeOutput> {
        let revoked = self.runtime.revoke(&input.handle_id, principal)?;
        Ok(SecretRevokeOutput {
            handle_id: input.handle_id,
            revoked,
        })
    }
}

impl AiTool for SecretRevokeTool {
    const ID: &'static str = SECRET_REVOKE_TOOL_ID;
    type Input = SecretRevokeInput;
    type Output = SecretRevokeOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            SECRET_REVOKE_TOOL_ID,
            "Revoke a session-scoped secret handle.",
        )
        .side_effecting()
    }

    fn call(
        &self,
        input: Self::Input,
        ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        let principal = ctx.principal().clone();
        Box::pin(async move { self.run(input, &principal) })
    }
}

pub fn register_secret_tools(
    builder: AgenkitBuilder,
    runtime: Arc<SecretRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    Ok(builder
        .tool(SecretListTool::new(runtime.clone()))
        .tool(SecretRequestTool::new(runtime.clone()))
        .tool(SecretUseTool::new(runtime.clone()))
        .tool(SecretRevokeTool::new(runtime)))
}

pub fn known_secret_tool_ids() -> [&'static str; 4] {
    [
        SECRET_LIST_TOOL_ID,
        SECRET_REQUEST_TOOL_ID,
        SECRET_USE_TOOL_ID,
        SECRET_REVOKE_TOOL_ID,
    ]
}
