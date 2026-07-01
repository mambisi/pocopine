//! `McpClientHandler`: services server→client requests (D11).
//!
//! MCP servers may call *back* into the client: `sampling/createMessage` (asks
//! our LLM to generate — T8), `elicitation/create` (asks the user for input —
//! T9), and `roots/list` (asks for the client's workspace roots). All three are
//! security-sensitive when the server is untrusted, so v1:
//!
//! - **denies `sampling/createMessage`** (returns `METHOD_NOT_FOUND`) — a server
//!   must never steer or burn tokens on our model (T8);
//! - **declines `elicitation/create`** (returns `action: decline`) — a server
//!   must never pop a fake "re-enter your API key" form (T9); and
//! - answers **`roots/list`** with the **configured** workspace roots only —
//!   never the host's full filesystem (D11).
//!
//! A `notifications/tools/list_changed` is **never silently adopted** (T2): the
//! handler **increments a shared, server-wide generation counter** (also held by
//! the owning [`McpConnection`](super::client::McpConnection) and every other
//! per-principal connection to the same server) so the runtime re-discovers +
//! re-pin-checks the server **before the next dispatch**, rather than continuing
//! to call tools under stale approvals. The generation is a monotonic counter —
//! **never consumed/cleared** — so each principal's connection observes the
//! notification independently (one dispatcher catching up its own connection
//! cannot clear the notification for another, RH2b-gen). The pinned-hash check at
//! call time then re-denies any changed/added/removed tool until it is re-approved.

// Sampling/Roots/elicitation primitives are on the SEP-2577 deprecation track,
// but the `ClientHandler` contract still requires servicing them — we deny /
// decline / answer-with-configured-roots by design (D11). Mirror rmcp's own
// `#![expect(deprecated)]` on its client handler.
#![allow(deprecated)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rmcp::ClientHandler;
use rmcp::ErrorData as McpError;
use rmcp::model::{
    ClientCapabilities, CreateMessageRequestMethod, CreateMessageRequestParams,
    CreateMessageResult, ElicitRequestParams, ElicitResult, ElicitationAction, Implementation,
    InitializeRequestParams, ListRootsResult, Root,
};
use rmcp::service::{MaybeSendFuture, NotificationContext, RequestContext, RoleClient};

/// Services server→client requests for one connection, denying the
/// security-sensitive primitives and exposing only the configured roots.
#[derive(Clone, Debug)]
pub struct McpClientHandler {
    /// The owning server name (trace context only).
    server_name: String,
    /// The configured workspace roots returned for `roots/list` (D11).
    roots: Vec<Root>,
    /// Shared, **server-wide** `tools/list_changed` generation counter, also held
    /// by the owning [`McpConnection`](super::client::McpConnection) and every
    /// other per-principal connection to the same server. **Incremented** on a
    /// `tools/list_changed` notification (never cleared/consumed) so the runtime
    /// re-discovers + re-pins the server before the next dispatch by **any**
    /// principal (T2/RH2b-gen). Each connection tracks its own observed generation
    /// and catches up independently, so one principal cannot consume the
    /// notification for another.
    server_generation: Arc<AtomicU64>,
}

impl McpClientHandler {
    /// Build a handler for `server_name` exposing exactly `roots`, sharing the
    /// server-wide `tools/list_changed` generation counter with the owning
    /// connection so a notification bumps it for the next dispatch (T2/RH2b-gen).
    pub fn new(
        server_name: impl Into<String>,
        roots: Vec<Root>,
        server_generation: Arc<AtomicU64>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            roots,
            server_generation,
        }
    }

    /// Bump the server-wide `tools/list_changed` generation (a notification was
    /// announced). Monotonic and never cleared, so every principal's connection
    /// observes it independently (RH2b-gen). Split out so it can be unit-tested
    /// without constructing a `NotificationContext`.
    pub fn note_tools_list_changed(&self) {
        // Saturating, NOT wrapping: at `u64::MAX` the counter pins (a permanent
        // "always-dirty" poison sentinel the convergence loop never treats as
        // caught up) instead of wrapping to 0. A naive `fetch_add(1)` would wrap,
        // and the convergence check `observed >= target` would then treat an
        // already-observed `u64::MAX` as caught up to the wrapped `0` — skipping
        // rediscovery and admitting under a stale pin. (Practically unreachable at
        // 2^64 notifications, but closed defensively — Codex round-5, Group I.)
        let _ = self
            .server_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |g| {
                Some(g.saturating_add(1))
            });
    }

    /// The `roots/list` answer: the configured workspace roots only.
    pub fn roots_result(&self) -> ListRootsResult {
        ListRootsResult::new(self.roots.clone())
    }

    /// The error returned for a denied `sampling/createMessage` (T8).
    pub fn sampling_denied() -> McpError {
        McpError::method_not_found::<CreateMessageRequestMethod>()
    }

    /// The result returned for a declined `elicitation/create` (T9).
    pub fn elicitation_declined() -> ElicitResult {
        ElicitResult::new(ElicitationAction::Decline)
    }
}

impl ClientHandler for McpClientHandler {
    fn create_message(
        &self,
        _params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<CreateMessageResult, McpError>> + MaybeSendFuture + '_ {
        tracing::warn!(
            target: "pocopine.log",
            server = self.server_name.as_str(),
            "mcp server requested sampling/createMessage; denied (v1 policy, T8)"
        );
        std::future::ready(Err(Self::sampling_denied()))
    }

    fn create_elicitation(
        &self,
        _request: ElicitRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<ElicitResult, McpError>> + MaybeSendFuture + '_ {
        tracing::warn!(
            target: "pocopine.log",
            server = self.server_name.as_str(),
            "mcp server requested elicitation/create; declined (v1 policy, T9)"
        );
        std::future::ready(Ok(Self::elicitation_declined()))
    }

    fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> impl Future<Output = Result<ListRootsResult, McpError>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(self.roots_result()))
    }

    fn on_tool_list_changed(
        &self,
        _context: NotificationContext<RoleClient>,
    ) -> impl Future<Output = ()> + MaybeSendFuture + '_ {
        // T2: never silently adopt a swapped tool set. Bump the shared server-wide
        // generation so the runtime re-discovers + re-pin-checks the server
        // *before* the next dispatch by any principal (a changed/added/removed tool
        // is then re-denied until re-approved). Incremented (never cleared) so the
        // notification is observed independently by every principal's connection
        // (RH2b-gen). Done synchronously here so it is visible regardless of when
        // the returned future is polled.
        self.note_tools_list_changed();
        tracing::warn!(
            target: "pocopine.log",
            server = self.server_name.as_str(),
            "mcp server sent tools/list_changed; bumped generation — re-discover + re-pin \
             before next dispatch (not auto-adopting, T2)"
        );
        std::future::ready(())
    }

    fn get_info(&self) -> InitializeRequestParams {
        // Advertise our identity + a minimal capability set. We deny sampling and
        // elicitation, so neither is advertised; roots are advertised because we
        // answer `roots/list` with the configured set.
        let mut capabilities = ClientCapabilities::default();
        capabilities.roots = Some(Default::default());
        // `InitializeRequestParams` is `#[non_exhaustive]`; build via its
        // constructor (protocol version defaults to our D3 target 2025-11-25).
        InitializeRequestParams::new(
            capabilities,
            Implementation::new("agenkitty", env!("CARGO_PKG_VERSION")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;

    #[test]
    fn sampling_is_denied_with_method_not_found() {
        let err = McpClientHandler::sampling_denied();
        assert_eq!(err.code, ErrorCode::METHOD_NOT_FOUND);
    }

    #[test]
    fn elicitation_is_declined() {
        let result = McpClientHandler::elicitation_declined();
        assert_eq!(result.action, ElicitationAction::Decline);
        assert!(result.content.is_none());
    }

    #[test]
    fn list_roots_returns_only_configured_roots() {
        let handler = McpClientHandler::new(
            "fixture",
            vec![Root::new("file:///workspace/project")],
            Arc::new(AtomicU64::new(0)),
        );
        let result = handler.roots_result();
        assert_eq!(result.roots.len(), 1);
        assert_eq!(result.roots[0].uri, "file:///workspace/project");
    }

    #[test]
    fn empty_roots_are_returned_when_none_configured() {
        let handler = McpClientHandler::new("fixture", Vec::new(), Arc::new(AtomicU64::new(0)));
        assert!(handler.roots_result().roots.is_empty());
    }

    #[test]
    fn tools_list_changed_bumps_the_shared_generation() {
        // The server-wide generation the handler shares with the owning connection
        // (RH2b-gen): a tools/list_changed notification INCREMENTS it (never
        // clears it) so the runtime re-discovers + re-pins before the next
        // dispatch, and so every principal's connection observes it independently.
        let generation = Arc::new(AtomicU64::new(0));
        let handler = McpClientHandler::new("fixture", Vec::new(), generation.clone());
        assert_eq!(generation.load(Ordering::Acquire), 0);
        handler.note_tools_list_changed();
        assert_eq!(
            generation.load(Ordering::Acquire),
            1,
            "a tools/list_changed must bump the server-wide generation"
        );
        // A second notification bumps again (monotonic, never consumed).
        handler.note_tools_list_changed();
        assert_eq!(generation.load(Ordering::Acquire), 2);
    }

    #[test]
    fn note_tools_list_changed_saturates_at_u64_max() {
        // Group I — the bump must SATURATE at `u64::MAX`, never wrap to 0. A naive
        // `fetch_add(1)` wraps; the convergence check `observed >= target` would
        // then treat an already-observed `u64::MAX` as caught up to the wrapped
        // `0`, skip rediscovery, and admit under a stale pin. Seeded at the
        // overflow boundary, a notification must leave the counter pinned at
        // `u64::MAX` (the permanent "always-dirty" poison sentinel) — NOT 0.
        let generation = Arc::new(AtomicU64::new(u64::MAX));
        let handler = McpClientHandler::new("fixture", Vec::new(), generation.clone());
        handler.note_tools_list_changed();
        assert_eq!(
            generation.load(Ordering::Acquire),
            u64::MAX,
            "a notification at u64::MAX must saturate (poison), never wrap to 0"
        );
    }
}
