use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a secret is scoped. This is metadata only; it does not imply storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecretScope {
    User,
    Workspace,
    Session,
}

/// Safe metadata the model may see. Labels/names must be chosen by the host so
/// they do not reveal secret values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretMetadata {
    pub secret_ref: String,
    pub label: String,
    pub scope: SecretScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub purposes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<String>,
}

impl SecretMetadata {
    pub fn new(
        secret_ref: impl Into<String>,
        label: impl Into<String>,
        scope: SecretScope,
    ) -> Self {
        Self {
            secret_ref: secret_ref.into(),
            label: label.into(),
            scope,
            purposes: Vec::new(),
            target_tools: Vec::new(),
            destinations: Vec::new(),
        }
    }

    pub fn with_purposes(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.purposes = values.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_target_tools(
        mut self,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.target_tools = values.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_destinations(
        mut self,
        values: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.destinations = values.into_iter().map(Into::into).collect();
        self
    }

    pub(crate) fn permits(
        &self,
        purpose: &str,
        target_tool: &str,
        destination: Option<&str>,
    ) -> bool {
        (self.purposes.is_empty() || self.purposes.iter().any(|value| value == purpose))
            && (self.target_tools.is_empty()
                || self.target_tools.iter().any(|value| value == target_tool))
            && (self.destinations.is_empty()
                || destination
                    .is_some_and(|dest| self.destinations.iter().any(|value| value == dest)))
    }
}

/// A serializable grant record. It intentionally carries only refs and policy
/// metadata, not the secret value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretGrant {
    pub handle_id: String,
    pub secret_ref: String,
    pub purpose: String,
    pub target_tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    pub inheritable: bool,
}
