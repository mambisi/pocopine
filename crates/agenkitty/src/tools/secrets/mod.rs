//! Scoped secret handles for host tools.
//!
//! This module keeps raw secret values behind a host-only resolver. The model
//! can list safe metadata, request an opaque handle, attach that handle to a
//! tool call, or revoke it. Only the target tool resolves the handle into a
//! [`SecretString`] at invocation time.

mod resolver;
mod runtime;
mod tools;
mod types;
mod validation;

pub use resolver::{
    AgentSecretResolver, EnvSecretResolver, InMemorySecretResolver, SecretListFuture,
    SecretResolveFuture,
};
pub use runtime::{SecretGrantKey, SecretRequestPolicy, SecretRuntime, current_time_ms};
pub use tools::{
    SECRET_LIST_TOOL_ID, SECRET_REQUEST_TOOL_ID, SECRET_REVOKE_TOOL_ID, SECRET_USE_TOOL_ID,
    SecretListInput, SecretListOutput, SecretListTool, SecretRequestInput, SecretRequestOutput,
    SecretRequestTool, SecretRevokeInput, SecretRevokeOutput, SecretRevokeTool, SecretUseInput,
    SecretUseOutput, SecretUseTool, known_secret_tool_ids, register_secret_tools,
};
pub use types::{SecretGrant, SecretMetadata, SecretScope};
pub use validation::{resolve_secret_headers, validate_secret_env_name};
