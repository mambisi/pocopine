//! Registration for the network tool family.
//!
//! Unlike `fs`/`process` (which register against a workspace root), network
//! registers against a [`NetPolicy`] — the host's domain allow/block-list. The
//! network capability must be granted separately; with no allowlist the tool
//! fetches nothing. `net.fetch` is side-effecting and opt-in, so it is not part
//! of any default tool set.

use pocopine_agenkit::server::AgenkitBuilder;
use pocopine_agenkit_core::AgenkitResult;
use std::sync::Arc;

use super::fetch::{NET_FETCH_TOOL_ID, NetFetchTool};
use super::policy::NetPolicy;
use crate::tools::secrets::SecretRuntime;

/// Register the network tool family bound to `policy`.
pub fn register_network_tools(
    builder: AgenkitBuilder,
    policy: NetPolicy,
) -> AgenkitResult<AgenkitBuilder> {
    let tool = NetFetchTool::new(policy)?;
    Ok(builder.tool(tool))
}

pub fn register_network_tools_with_secrets(
    builder: AgenkitBuilder,
    policy: NetPolicy,
    secret_runtime: Arc<SecretRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    let tool = NetFetchTool::new(policy)?.with_secret_runtime(secret_runtime);
    Ok(builder.tool(tool))
}

/// The tool ids in this family.
pub fn known_network_tool_ids() -> [&'static str; 1] {
    [NET_FETCH_TOOL_ID]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_contain_net_fetch() {
        assert!(known_network_tool_ids().contains(&NET_FETCH_TOOL_ID));
    }
}
