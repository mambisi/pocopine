//! Bounded outbound network tools.
//!
//! The README captures the public contract and deferred roadmap. The security
//! core is [`ssrf`]: every request passes [`ssrf::guard`] (resolve → validate →
//! pin) before any byte leaves the host. [`policy::NetPolicy`] is the host
//! allow/block-list layered on top. Host-only (the parent `tools` module is
//! already gated to non-wasm targets).

pub mod error;
pub mod fetch;
pub mod policy;
pub mod proxy;
pub mod registry;
pub mod ssrf;

pub use error::{NetError, NetErrorCode, NetResult};
pub use fetch::{HickoryResolver, NET_FETCH_TOOL_ID, NetFetchInput, NetFetchOutput, NetFetchTool};
pub use policy::NetPolicy;
pub use proxy::EgressProxy;
pub use registry::{
    known_network_tool_ids, register_network_tools, register_network_tools_with_secrets,
};
pub use ssrf::{
    MAX_URL_LEN, Resolve, UrlTarget, ValidatedTarget, guard, ip_is_blocked, normalize_host,
    resolve_and_pin, validate_url,
};
