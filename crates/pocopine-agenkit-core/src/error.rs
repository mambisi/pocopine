//! The unified error type for Agenkit (RFC-093 §D13, §D15 DC-2).
//!
//! Modelled as a manual enum + `Display` + `std::error::Error` + helper
//! constructors + `impl From`, matching the workspace convention
//! (`JobError`/`SyncError`/`StorageError`); no `thiserror`. The variants
//! distinguish the failure classes a flow runtime must tell apart so callers,
//! traces, and the server boundary can react differently.

use std::fmt;

/// Convenient `Result` alias for Agenkit APIs.
pub type AgenkitResult<T> = Result<T, AgenkitError>;

/// Every way an Agenkit operation can fail.
///
/// Serializable so it can ride trace payloads and be asserted in tests; the
/// facade maps it to `pocopine_server::ServerError` before it crosses the
/// client boundary (provider internals never leak).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgenkitError {
    /// A model provider call failed (network, provider error, bad response).
    Provider { message: String },
    /// The request exceeded the model's context window (input + requested output
    /// too large). A refined provider failure callers can branch on to compact
    /// and retry, rather than treating it as an opaque provider error (W3).
    ContextOverflow { message: String },
    /// Structured output or a tool argument failed schema validation.
    Validation { message: String },
    /// A tool was rejected by policy: not allowlisted, side-effect not
    /// permitted, or principal not authorized.
    ToolPolicy { message: String },
    /// A per-flow/per-step budget was exhausted (tokens, time, cost, depth).
    BudgetExhausted { message: String },
    /// The operation was cancelled (flow cancelled, losing parallel branch).
    Cancelled,
    /// A reducer could not accept any candidate, or branches disagreed beyond
    /// the configured success policy.
    ReducerDisagreement { message: String },
    /// Misconfiguration: unknown provider/model alias, missing registration,
    /// or an invalid builder.
    Config { message: String },
    /// A referenced resource (flow, agent, thread, tool, retriever) was not
    /// found in the registry.
    NotFound { message: String },
    /// Serialization/deserialization of a payload failed.
    Json { message: String },
    /// A runtime invariant broke (e.g. a parallel branch panicked). Never a
    /// caller mistake; indicates a bug in app or framework code.
    Internal { message: String },
}

impl AgenkitError {
    /// A model provider failure.
    pub fn provider(message: impl Into<String>) -> Self {
        Self::Provider {
            message: message.into(),
        }
    }

    /// A context-window-overflow failure (request too long for the model).
    pub fn context_overflow(message: impl Into<String>) -> Self {
        Self::ContextOverflow {
            message: message.into(),
        }
    }

    /// A schema/validation failure.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation {
            message: message.into(),
        }
    }

    /// A tool policy rejection.
    pub fn tool_policy(message: impl Into<String>) -> Self {
        Self::ToolPolicy {
            message: message.into(),
        }
    }

    /// A budget-exhaustion failure.
    pub fn budget_exhausted(message: impl Into<String>) -> Self {
        Self::BudgetExhausted {
            message: message.into(),
        }
    }

    /// A reducer disagreement / no-acceptable-candidate failure.
    pub fn reducer_disagreement(message: impl Into<String>) -> Self {
        Self::ReducerDisagreement {
            message: message.into(),
        }
    }

    /// A configuration failure.
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
        }
    }

    /// A registry lookup miss.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::NotFound {
            message: message.into(),
        }
    }

    /// A broken runtime invariant (e.g. a panicked parallel branch).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    /// The stable snake_case discriminant, useful for trace fields and tests.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Provider { .. } => "provider",
            Self::ContextOverflow { .. } => "context_overflow",
            Self::Validation { .. } => "validation",
            Self::ToolPolicy { .. } => "tool_policy",
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::Cancelled => "cancelled",
            Self::ReducerDisagreement { .. } => "reducer_disagreement",
            Self::Config { .. } => "config",
            Self::NotFound { .. } => "not_found",
            Self::Json { .. } => "json",
            Self::Internal { .. } => "internal",
        }
    }

    /// Whether a retry could plausibly succeed (transient provider failures).
    /// Validation, policy, and config errors are not retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Provider { .. })
    }
}

impl fmt::Display for AgenkitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider { message } => write!(f, "provider error: {message}"),
            Self::ContextOverflow { message } => write!(f, "context overflow: {message}"),
            Self::Validation { message } => write!(f, "validation error: {message}"),
            Self::ToolPolicy { message } => write!(f, "tool policy error: {message}"),
            Self::BudgetExhausted { message } => write!(f, "budget exhausted: {message}"),
            Self::Cancelled => f.write_str("operation cancelled"),
            Self::ReducerDisagreement { message } => {
                write!(f, "reducer disagreement: {message}")
            }
            Self::Config { message } => write!(f, "configuration error: {message}"),
            Self::NotFound { message } => write!(f, "not found: {message}"),
            Self::Json { message } => write!(f, "serialization error: {message}"),
            Self::Internal { message } => write!(f, "internal error: {message}"),
        }
    }
}

impl std::error::Error for AgenkitError {}

impl From<serde_json::Error> for AgenkitError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json {
            message: err.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_set_kind() {
        assert_eq!(AgenkitError::provider("x").kind(), "provider");
        assert_eq!(AgenkitError::validation("x").kind(), "validation");
        assert_eq!(AgenkitError::Cancelled.kind(), "cancelled");
    }

    #[test]
    fn only_provider_errors_are_retryable() {
        assert!(AgenkitError::provider("timeout").is_retryable());
        assert!(!AgenkitError::validation("bad").is_retryable());
        assert!(!AgenkitError::Cancelled.is_retryable());
    }

    #[test]
    fn context_overflow_has_its_own_kind_and_is_not_retryable() {
        let e = AgenkitError::context_overflow("prompt is too long");
        assert_eq!(e.kind(), "context_overflow");
        assert!(!e.is_retryable());
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"kind\":\"context_overflow\""));
        assert_eq!(serde_json::from_str::<AgenkitError>(&json).unwrap(), e);
    }

    #[test]
    fn serde_round_trip_is_tagged() {
        let err = AgenkitError::tool_policy("not allowlisted");
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"kind\":\"tool_policy\""));
        let back: AgenkitError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn json_error_maps_through_from() {
        let err: AgenkitError = serde_json::from_str::<i32>("not json").unwrap_err().into();
        assert_eq!(err.kind(), "json");
    }
}
