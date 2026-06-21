use std::collections::BTreeMap;

use serde_json::Value;

/// Type alias for the wire-level stream parameter map used by shape
/// subscriptions. Sorted by key so the canonical JSON encoding (and
/// derived cache hash) are deterministic across builds and clients.
pub type StreamParams = BTreeMap<String, Value>;

mod hashing;
mod mutation;
mod names;
mod request;
mod response;
mod wire;

pub use hashing::*;
pub use mutation::*;
pub use names::*;
pub use request::*;
pub use response::*;
pub use wire::*;

// Non-`pub` deserializer helpers shared across sibling modules. The
// glob re-exports above only cover `pub` items, so these `pub(crate)`
// helpers are re-exported explicitly so each child's `use super::…`
// (and external `crate::protocol::…` paths) keep resolving.
pub(crate) use request::deserialize_params_null_as_default;
pub(crate) use response::deserialize_schema_version_default_one;
