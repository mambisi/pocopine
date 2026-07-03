//! Registration for the network tool family.
//!
//! Unlike `fs`/`process` (which register against a workspace root), network
//! registers against a [`NetPolicy`] — the host's domain allow/block-list. The
//! network capability must be granted separately; with no allowlist the tools
//! fetch nothing. `net.fetch` and `net.download` are side-effecting and opt-in,
//! so they are not part of any default tool set. `net.download` additionally
//! needs an [`ArtifactRuntime`] to store into.

use pocopine_agenkit::server::AgenkitBuilder;
use pocopine_agenkit_core::AgenkitResult;
use std::sync::Arc;

use super::download::{NET_DOWNLOAD_TOOL_ID, NetDownloadTool};
use super::fetch::{NET_FETCH_TOOL_ID, NetFetchTool};
use super::policy::NetPolicy;
use super::resolve::{NET_RESOLVE_TOOL_ID, NetResolveTool};
use crate::tools::artifacts::ArtifactRuntime;
use crate::tools::secrets::SecretRuntime;

/// Register `net.fetch` + the `net.resolve` pre-flight, bound to `policy`.
pub fn register_network_tools(
    builder: AgenkitBuilder,
    policy: NetPolicy,
) -> AgenkitResult<AgenkitBuilder> {
    let fetch = NetFetchTool::new(policy.clone())?;
    let resolve = NetResolveTool::new(policy)?;
    Ok(builder.tool(fetch).tool(resolve))
}

pub fn register_network_tools_with_secrets(
    builder: AgenkitBuilder,
    policy: NetPolicy,
    secret_runtime: Arc<SecretRuntime>,
) -> AgenkitResult<AgenkitBuilder> {
    let fetch = NetFetchTool::new(policy.clone())?.with_secret_runtime(secret_runtime);
    let resolve = NetResolveTool::new(policy)?;
    Ok(builder.tool(fetch).tool(resolve))
}

/// Register `net.fetch` **and** `net.download` bound to `policy`, with
/// `net.download` storing into `artifacts`. `secret_runtime`, when provided,
/// backs secret-handle request headers on both tools.
pub fn register_network_tools_with_artifacts(
    builder: AgenkitBuilder,
    policy: NetPolicy,
    artifacts: Arc<ArtifactRuntime>,
    secret_runtime: Option<Arc<SecretRuntime>>,
) -> AgenkitResult<AgenkitBuilder> {
    let mut fetch = NetFetchTool::new(policy.clone())?;
    let resolve = NetResolveTool::new(policy.clone())?;
    let mut download = NetDownloadTool::new(policy, artifacts)?;
    if let Some(secret_runtime) = secret_runtime {
        fetch = fetch.with_secret_runtime(secret_runtime.clone());
        download = download.with_secret_runtime(secret_runtime);
    }
    Ok(builder.tool(fetch).tool(resolve).tool(download))
}

/// The tool ids in this family.
pub fn known_network_tool_ids() -> [&'static str; 3] {
    [NET_FETCH_TOOL_ID, NET_RESOLVE_TOOL_ID, NET_DOWNLOAD_TOOL_ID]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_contain_the_verbs() {
        assert!(known_network_tool_ids().contains(&NET_FETCH_TOOL_ID));
        assert!(known_network_tool_ids().contains(&NET_RESOLVE_TOOL_ID));
        assert!(known_network_tool_ids().contains(&NET_DOWNLOAD_TOOL_ID));
    }
}
