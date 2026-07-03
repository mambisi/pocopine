//! `net.resolve` — pre-flight one allowlisted URL through the SSRF guard
//! without fetching it.
//!
//! It runs the exact same admission as `net.fetch`/`net.download` (scheme +
//! host + port allowlist → DNS → private/metadata block → pin) but stops before
//! connecting, so the agent can check reachability and see where an allowlisted
//! host resolves. A non-allowlisted or blocked target errors here just as a
//! fetch would, and — like a fetch — never echoes a blocked address (no
//! internal-network reconnaissance oracle). A success is proof the host is
//! allowlisted AND resolves entirely to non-blocked (public) addresses, so
//! surfacing those addresses is safe.

use pocopine_agenkit::server::{AiTool, AiToolContext, BoxFuture};
use pocopine_agenkit_core::{AgenkitResult, ToolDescriptor};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::error::NetResult;
use super::http::GuardedHttp;
use super::policy::NetPolicy;
use super::ssrf::Resolve;

pub const NET_RESOLVE_TOOL_ID: &str = "net.resolve";

/// The `net.resolve` tool — a read-only pre-flight over the shared
/// [`GuardedHttp`] engine (same allow/block policy + DNS resolver as
/// `net.fetch`). Defaults to deny-everything (no allowlist).
pub struct NetResolveTool {
    http: GuardedHttp,
}

impl NetResolveTool {
    /// Build with the production hickory resolver.
    pub fn new(policy: NetPolicy) -> NetResult<Self> {
        Ok(Self {
            http: GuardedHttp::new(policy)?,
        })
    }

    /// Build with an injected resolver (tests).
    pub fn with_resolver(policy: NetPolicy, resolver: Arc<dyn Resolve>) -> Self {
        Self {
            http: GuardedHttp::with_resolver(policy, resolver),
        }
    }

    async fn resolve(&self, input: NetResolveInput) -> NetResult<NetResolveOutput> {
        let target = self.http.resolve(&input.url).await?;
        // Bare IPs — the shared port is already returned as its own field.
        let addresses: Vec<String> = target
            .addrs()
            .iter()
            .map(|addr| addr.ip().to_string())
            .collect();
        // Host + count only (never the full URL — a query can carry secrets).
        tracing::info!(
            target: "pocopine.log",
            tool = NET_RESOLVE_TOOL_ID,
            host = %target.host(),
            addresses = addresses.len(),
            "net.resolve completed"
        );
        Ok(NetResolveOutput {
            url: target.url().to_string(),
            host: target.host().to_string(),
            port: target.port(),
            addresses,
        })
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct NetResolveInput {
    /// The URL to resolve. Must be allowlisted and pass the SSRF guard; https
    /// only by default. Nothing is fetched.
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct NetResolveOutput {
    /// The host-normalized URL that would be requested.
    pub url: String,
    /// The canonical host (the allowlist + pin key).
    pub host: String,
    /// The destination port (shared by every address).
    pub port: u16,
    /// The validated IP addresses the host resolved to (all non-blocked/public).
    pub addresses: Vec<String>,
}

impl AiTool for NetResolveTool {
    const ID: &'static str = NET_RESOLVE_TOOL_ID;
    type Input = NetResolveInput;
    type Output = NetResolveOutput;

    fn descriptor() -> ToolDescriptor {
        ToolDescriptor::new(
            NET_RESOLVE_TOOL_ID,
            "Resolve one allowlisted URL through the SSRF guard WITHOUT fetching it, \
             returning the canonical host, port, and validated (public) addresses. \
             Private, metadata, and internal network targets are always blocked. Use \
             this to pre-flight reachability before net.fetch/net.download.",
        )
    }

    fn call(
        &self,
        input: Self::Input,
        _ctx: AiToolContext,
    ) -> BoxFuture<'_, AgenkitResult<Self::Output>> {
        Box::pin(async move { self.resolve(input).await.map_err(Into::into) })
    }
}

#[cfg(test)]
mod tests {
    use super::super::error::NetErrorCode;
    use super::super::ssrf::Resolve;
    use super::*;
    use pocopine_agenkit::server::BoxFuture;
    use pocopine_agenkit_core::ToolSideEffectPolicy;
    use std::net::IpAddr;

    /// A resolver returning fixed addresses for any host.
    struct FixedResolver(Vec<IpAddr>);
    impl Resolve for FixedResolver {
        fn lookup(&self, _host: &str) -> BoxFuture<'_, NetResult<Vec<IpAddr>>> {
            let ips = self.0.clone();
            Box::pin(async move { Ok(ips) })
        }
    }

    fn tool(policy: NetPolicy, ips: Vec<IpAddr>) -> NetResolveTool {
        NetResolveTool::with_resolver(policy, Arc::new(FixedResolver(ips)))
    }

    #[tokio::test]
    async fn resolves_an_allowlisted_host_to_its_public_addresses() {
        let policy = NetPolicy::allow(["example.com"]);
        let out = tool(policy, vec!["93.184.216.34".parse().unwrap()])
            .resolve(NetResolveInput {
                url: "https://example.com/path".to_string(),
            })
            .await
            .unwrap();
        assert_eq!(out.host, "example.com");
        assert_eq!(out.port, 443);
        assert_eq!(out.addresses, vec!["93.184.216.34".to_string()]);
    }

    #[tokio::test]
    async fn refuses_a_non_allowlisted_host() {
        let err = tool(NetPolicy::default(), vec!["93.184.216.34".parse().unwrap()])
            .resolve(NetResolveInput {
                url: "https://not-allowed.example/".to_string(),
            })
            .await
            .unwrap_err();
        assert!(err.message.contains("allowlist"));
    }

    #[tokio::test]
    async fn blocks_an_allowlisted_host_that_resolves_to_a_private_address() {
        // Allowlisted, but DNS points at a metadata/private address: blocked,
        // and the refusal never echoes the blocked IP.
        let policy = NetPolicy::allow(["sneaky.example"]);
        let err = tool(policy, vec!["169.254.169.254".parse().unwrap()])
            .resolve(NetResolveInput {
                url: "https://sneaky.example/".to_string(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, NetErrorCode::UrlNotAllowed);
        assert!(!err.message.contains("169.254"));
    }

    #[tokio::test]
    async fn resolve_charges_the_request_budget() {
        // A resolve does real DNS, so it must count against max_requests — or
        // net.resolve would be an uncapped DNS channel that net.fetch bounds.
        let policy = NetPolicy::allow(["example.com"]).with_max_requests(1);
        let tool = tool(policy, vec!["93.184.216.34".parse().unwrap()]);
        // First resolve consumes the single-request budget.
        tool.resolve(NetResolveInput {
            url: "https://example.com/".to_string(),
        })
        .await
        .unwrap();
        // The second is refused by the budget, exactly as a fetch would be.
        let err = tool
            .resolve(NetResolveInput {
                url: "https://example.com/other".to_string(),
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, NetErrorCode::TooManyRequests);
    }

    #[test]
    fn resolve_descriptor_is_not_side_effecting() {
        // A pure pre-flight: no body is fetched, so it is not side-effecting.
        assert_ne!(
            NetResolveTool::descriptor().side_effect,
            ToolSideEffectPolicy::SideEffecting
        );
    }
}
