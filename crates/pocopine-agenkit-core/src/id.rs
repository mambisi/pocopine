//! Identifier newtypes (RFC-093 Phase 1.1).
//!
//! Core only *carries* ids as transparent string newtypes; the facade mints
//! them (via `pocopine-crypto`/`pocopine-codec`) so this crate stays
//! dependency-light. [`ModelRef`] is special: it is a `"provider/model"`
//! alias, never a raw provider id (§D10).

/// Declare a transparent `String` newtype id with the standard surface.
macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a value as this id.
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Borrow the underlying string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume into the owned string.
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(
    /// A provider alias (e.g. `"local"`, `"openai"`). Never a credential.
    ProviderRef
);
string_id!(
    /// A single flow/agent invocation id.
    RunId
);
string_id!(
    /// A correlation id stamped onto every trace event in one run tree.
    TraceId
);
string_id!(
    /// An opaque session id (chat/debugger continuity).
    SessionId
);
string_id!(
    /// An opaque agent-thread id. Never a provider-backed thread id (§D10).
    AgentThreadId
);
string_id!(
    /// Groups the branches of one `ctx.parallel(...)` fan-out.
    ParallelGroupId
);
string_id!(
    /// Identifies one step within a flow's trace tree.
    StepId
);

/// A model alias of the shape `"provider/model"` (e.g. `"local/default"`).
///
/// The server allowlists aliases per app/flow before resolving them to a
/// host-side provider; clients never send raw provider ids (§D10).
///
/// Holds a `Cow<'static, str>` so a known model can be a `const` — the
/// generated `models` handles (`models::anthropic::CLAUDE_OPUS_4_8`) are
/// borrowed-`'static`, while [`ModelRef::new`] owns an arbitrary alias (an
/// open-weight model behind a gateway the catalog doesn't index).
#[derive(
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub struct ModelRef(std::borrow::Cow<'static, str>);

impl ModelRef {
    /// Wrap a `"provider/model"` alias (owns the string).
    pub fn new(value: impl Into<String>) -> Self {
        Self(std::borrow::Cow::Owned(value.into()))
    }

    /// Wrap a `'static` `"provider/model"` alias without allocating — the
    /// `const` constructor the generated [`models`](crate) handles use.
    pub const fn from_static(value: &'static str) -> Self {
        Self(std::borrow::Cow::Borrowed(value))
    }

    /// Borrow the full alias string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The provider-alias portion (before the first `/`), if the ref is
    /// namespaced.
    pub fn provider(&self) -> Option<&str> {
        let s: &str = &self.0;
        s.split_once('/').map(|(provider, _)| provider)
    }

    /// The model portion (after the first `/`), or the whole string when the
    /// ref is not namespaced.
    pub fn model(&self) -> &str {
        let s: &str = &self.0;
        s.split_once('/').map_or(s, |(_, model)| model)
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ModelRef {
    fn from(value: &str) -> Self {
        Self(std::borrow::Cow::Owned(value.to_owned()))
    }
}

impl From<String> for ModelRef {
    fn from(value: String) -> Self {
        Self(std::borrow::Cow::Owned(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_id_round_trips_transparently() {
        let id = StepId::new("step-1");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"step-1\"");
        let back: StepId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
        assert_eq!(id.as_str(), "step-1");
    }

    #[test]
    fn model_ref_splits_provider_and_model() {
        let m = ModelRef::new("local/default");
        assert_eq!(m.provider(), Some("local"));
        assert_eq!(m.model(), "default");
        assert_eq!(m.as_str(), "local/default");
    }

    #[test]
    fn model_ref_without_namespace_returns_whole_as_model() {
        let m = ModelRef::new("default");
        assert_eq!(m.provider(), None);
        assert_eq!(m.model(), "default");
    }

    #[test]
    fn model_ref_from_static_is_const_and_equals_owned() {
        // A generated handle is a `const` borrowed ref; it must compare/serialize
        // exactly like the owned form so it's a drop-in alias.
        const HANDLE: ModelRef = ModelRef::from_static("anthropic/claude-opus-4-8");
        assert_eq!(HANDLE, ModelRef::new("anthropic/claude-opus-4-8"));
        assert_eq!(HANDLE.provider(), Some("anthropic"));
        assert_eq!(HANDLE.model(), "claude-opus-4-8");
        assert_eq!(
            serde_json::to_string(&HANDLE).unwrap(),
            "\"anthropic/claude-opus-4-8\""
        );
    }
}
