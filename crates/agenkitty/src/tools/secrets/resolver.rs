use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use pocopine_agenkit::server::{Principal, SecretString};
use pocopine_agenkit_core::AgenkitResult;

use super::types::SecretMetadata;

/// Object-safe async return shape for [`AgentSecretResolver`] metadata reads.
pub type SecretListFuture<'a> =
    Pin<Box<dyn Future<Output = AgenkitResult<Vec<SecretMetadata>>> + Send + 'a>>;

/// Object-safe async return shape for late-bound secret resolution.
pub type SecretResolveFuture<'a> =
    Pin<Box<dyn Future<Output = AgenkitResult<Option<SecretString>>> + Send + 'a>>;

/// Host-only resolver. Implementations may read env vars, a local keychain, a
/// database, or a private product vault. The trait stays out of `agenkitty-core`
/// and never serializes raw values.
pub trait AgentSecretResolver: Send + Sync + 'static {
    fn list<'a>(&'a self, principal: &'a Principal) -> SecretListFuture<'a>;

    fn resolve<'a>(
        &'a self,
        secret_ref: &'a str,
        principal: &'a Principal,
        purpose: &'a str,
        target_tool: &'a str,
        destination: Option<&'a str>,
    ) -> SecretResolveFuture<'a>;
}

/// Local/dev in-memory resolver. Useful for tests and embedders that already
/// have values in memory. Values remain host-only `SecretString`s.
#[derive(Default)]
pub struct InMemorySecretResolver {
    entries: BTreeMap<String, InMemorySecret>,
}

struct InMemorySecret {
    metadata: SecretMetadata,
    value: SecretString,
}

impl InMemorySecretResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(mut self, metadata: SecretMetadata, value: impl Into<SecretString>) -> Self {
        self.entries.insert(
            metadata.secret_ref.clone(),
            InMemorySecret {
                metadata,
                value: value.into(),
            },
        );
        self
    }
}

impl AgentSecretResolver for InMemorySecretResolver {
    fn list<'a>(&'a self, _principal: &'a Principal) -> SecretListFuture<'a> {
        Box::pin(async move {
            Ok(self
                .entries
                .values()
                .map(|entry| entry.metadata.clone())
                .collect())
        })
    }

    fn resolve<'a>(
        &'a self,
        secret_ref: &'a str,
        _principal: &'a Principal,
        purpose: &'a str,
        target_tool: &'a str,
        destination: Option<&'a str>,
    ) -> SecretResolveFuture<'a> {
        Box::pin(async move {
            let Some(entry) = self.entries.get(secret_ref) else {
                return Ok(None);
            };
            if entry.metadata.permits(purpose, target_tool, destination) {
                Ok(Some(entry.value.clone()))
            } else {
                Ok(None)
            }
        })
    }
}

/// Local/dev environment-backed resolver. Only explicitly registered refs can
/// be resolved; the host environment is never copied wholesale.
#[derive(Default)]
pub struct EnvSecretResolver {
    entries: BTreeMap<String, EnvSecret>,
}

struct EnvSecret {
    metadata: SecretMetadata,
    env_var: String,
}

impl EnvSecretResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(mut self, metadata: SecretMetadata, env_var: impl Into<String>) -> Self {
        self.entries.insert(
            metadata.secret_ref.clone(),
            EnvSecret {
                metadata,
                env_var: env_var.into(),
            },
        );
        self
    }
}

impl AgentSecretResolver for EnvSecretResolver {
    fn list<'a>(&'a self, _principal: &'a Principal) -> SecretListFuture<'a> {
        Box::pin(async move {
            Ok(self
                .entries
                .values()
                .map(|entry| entry.metadata.clone())
                .collect())
        })
    }

    fn resolve<'a>(
        &'a self,
        secret_ref: &'a str,
        _principal: &'a Principal,
        purpose: &'a str,
        target_tool: &'a str,
        destination: Option<&'a str>,
    ) -> SecretResolveFuture<'a> {
        Box::pin(async move {
            let Some(entry) = self.entries.get(secret_ref) else {
                return Ok(None);
            };
            if !entry.metadata.permits(purpose, target_tool, destination) {
                return Ok(None);
            }
            Ok(std::env::var(&entry.env_var).ok().map(SecretString::new))
        })
    }
}
