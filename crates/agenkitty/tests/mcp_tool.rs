//! Integration tests for the MCP stdio transport (Phase 1).
//!
//! These drive the **real connection path**: spawn the `mcp_fixture_server`
//! test binary through agenkitty's process sandbox, run the rmcp handshake over
//! the child's `(stdout, stdin)`, validate the negotiated protocol version, and
//! discover tools with cursor pagination. The whole module is unix-gated (the
//! stdio transport reuses the unix-only process sandbox).
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use agenkitty::policy::{ApprovalDecision, ApprovalRequest, ToolApprover};
use agenkitty::tools::mcp::{
    CapabilitySet, McpAuditKind, McpConnection, McpObserver, McpRuntime, McpServerConfig,
    McpToolAdapter, McpToolsTool, McpTransportConfig, NetworkCapability, PinStatus,
    register_mcp_tools,
};
use pocopine_agenkit::prelude::Agenkit;
use pocopine_agenkit::server::{AuthUser, BoxFuture, MockProvider, Principal, SecretString};
use tempfile::TempDir;

/// A scripted approver: answers every request with the configured decision and
/// records the requests it saw.
struct ScriptedApprover {
    approve: bool,
    seen: std::sync::Mutex<Vec<ApprovalRequest>>,
}

impl ScriptedApprover {
    fn new(approve: bool) -> Self {
        Self {
            approve,
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl ToolApprover for ScriptedApprover {
    fn approve<'a>(&'a self, request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(request);
            if self.approve {
                ApprovalDecision::Approved
            } else {
                ApprovalDecision::denied("denied by operator")
            }
        })
    }
}

/// Absolute path to the compiled fixture server bin (cargo sets this for tests).
const FIXTURE: &str = env!("CARGO_BIN_EXE_mcp_fixture_server");

fn host_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string())
}

/// A canonical, writable workspace root for the sandbox.
fn workspace() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

/// A stdio server config pointing at the fixture with the given argv tail.
fn fixture_config(args: &[&str]) -> McpServerConfig {
    McpServerConfig {
        name: "fixture".to_string(),
        transport: McpTransportConfig::Stdio {
            command: FIXTURE.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
            cwd: None,
        },
        ..McpServerConfig::stdio("fixture", FIXTURE)
    }
}

#[tokio::test]
async fn handshake_negotiates_and_discovers_paginated_tools() {
    let (_dir, root) = workspace();
    let conn = McpConnection::connect(&fixture_config(&[]), &root, &host_path())
        .await
        .expect("connect should succeed against the fixture");

    assert_eq!(conn.protocol_version(), "2025-11-25");
    assert_eq!(conn.server_info().name, "mcp_fixture_server");

    // Two pages (`alpha` + `nextCursor` → `beta`) prove cursor pagination.
    let names: Vec<&str> = conn.tools().iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["alpha", "beta"]);
    assert_eq!(
        conn.tools()[0].description.as_deref(),
        Some("first tool"),
        "descriptions are captured (bounded)"
    );

    conn.close().await.unwrap();
}

#[tokio::test]
async fn unsupported_protocol_version_is_a_config_error() {
    let (_dir, root) = workspace();
    let config = fixture_config(&["--protocol", "1999-01-01"]);
    // `McpConnection` is not `Debug` (it owns an rmcp `RunningService`), so go
    // through `.err()` rather than `expect_err`; an unexpected `Ok` drops the
    // connection, which group-kills the child on `Drop`.
    let err = McpConnection::connect(&config, &root, &host_path())
        .await
        .err()
        .expect("an unsupported protocol version must be rejected");
    assert_eq!(err.kind(), "config");
    assert!(
        err.to_string().contains("1999-01-01"),
        "the error should name the offending version: {err}"
    );
}

#[tokio::test]
async fn child_env_is_scrubbed() {
    let (_dir, root) = workspace();
    // A canary in THIS process's environment must not leak into the child.
    // SAFETY: test-only; the var name is unique to this test.
    unsafe { std::env::set_var("MCP_LEAK_CANARY", "leaked-secret-value") };

    let config = fixture_config(&["--reflect-env", "MCP_LEAK_CANARY,PATH"]);
    let conn = McpConnection::connect(&config, &root, &host_path())
        .await
        .expect("connect should succeed");
    let instructions = conn.instructions().unwrap_or("").to_string();
    conn.close().await.unwrap();

    // env_clear means the child never inherited the canary.
    assert!(
        instructions.contains("env:MCP_LEAK_CANARY=__unset__"),
        "child must not inherit the host env: {instructions:?}"
    );
    assert!(
        !instructions.contains("leaked-secret-value"),
        "the secret value must not reach the child: {instructions:?}"
    );
    // PATH is the explicit host-provided one, not whatever the child would
    // otherwise inherit.
    assert!(
        instructions.contains(&format!("env:PATH={}", host_path())),
        "PATH should be the explicit sandbox PATH: {instructions:?}"
    );
}

/// On Linux the seccomp filter denies `AF_INET`/`AF_INET6` sockets when the
/// server has no network capability, so the child's connect attempt fails at
/// `socket()` (the namespace/seccomp is the control, not reachability).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn no_network_without_capability() {
    let (_dir, root) = workspace();
    let config = fixture_config(&["--try-connect", "127.0.0.1:9"]);
    let conn = McpConnection::connect(&config, &root, &host_path())
        .await
        .expect("connect should succeed (the server itself runs fine)");
    let instructions = conn.instructions().unwrap_or("").to_string();
    conn.close().await.unwrap();

    assert!(
        instructions.contains("net:blocked"),
        "INET sockets must be denied without a network capability: {instructions:?}"
    );
}

/// A stdio server that declares a network host allow/deny list fails closed: the
/// stdio seccomp sandbox can't do host-level egress filtering and no egress proxy
/// is wired for stdio in v1, so rather than granting unrestricted INET (which
/// would reach RFC1918 / the cloud metadata endpoint / exfil hosts) the
/// connection is refused with a `tool_policy` error.
#[tokio::test]
async fn network_host_list_is_rejected_for_stdio() {
    let (_dir, root) = workspace();
    let mut config = fixture_config(&["--try-connect", "169.254.169.254:80"]);
    config.capabilities = CapabilitySet {
        network: Some(NetworkCapability {
            allow_hosts: vec!["api.example.com".to_string()],
            deny_hosts: vec![],
        }),
        ..CapabilitySet::default()
    };
    let err = McpConnection::connect(&config, &root, &host_path())
        .await
        .err()
        .expect("a stdio server with an unenforceable network host list must be refused");
    assert_eq!(err.kind(), "tool_policy");
}

/// An empty `network: {}` capability is "deny all" (the documented contract): it
/// grants no egress, so the stdio child still has no INET access (seccomp blocks
/// `socket(AF_INET)`) rather than the previous silent unrestricted-network grant.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn empty_network_capability_grants_no_network() {
    let (_dir, root) = workspace();
    let mut config = fixture_config(&["--try-connect", "127.0.0.1:9"]);
    config.capabilities = CapabilitySet {
        network: Some(NetworkCapability::default()),
        ..CapabilitySet::default()
    };
    let conn = McpConnection::connect(&config, &root, &host_path())
        .await
        .expect("connect should succeed (the server itself runs fine)");
    let instructions = conn.instructions().unwrap_or("").to_string();
    conn.close().await.unwrap();

    // `network: {}` = deny all → INET sockets are still seccomp-blocked.
    assert!(
        instructions.contains("net:blocked"),
        "an empty network capability must NOT grant INET (deny-all contract): {instructions:?}"
    );
}

/// A hostile stdio server handed a per-server secret in its env can echo it to
/// stderr then fail the handshake. The resulting connect error embeds the
/// captured stderr — it must be scrubbed of the injected secret value so the
/// secret never reaches the model transcript / trace (D6/T4).
#[tokio::test]
async fn handshake_error_redacts_injected_secret_from_stderr() {
    let (_dir, root) = workspace();
    let secret = "ghp_leaked_handshake_secret_value";
    let config = fixture_config(&["--leak-env-stderr", "MCP_SECRET", "--fail-init"]);

    // Inject the resolved secret directly as the per-server child env (the runtime
    // would normally resolve it from a handle); it is also the redaction set.
    let mut secrets = BTreeMap::new();
    secrets.insert("MCP_SECRET".to_string(), SecretString::new(secret));

    let err = McpConnection::connect_with_secrets(&config, &root, &host_path(), secrets)
        .await
        .err()
        .expect("a failed handshake must surface as an error");
    let msg = err.to_string();
    assert!(
        msg.contains("[REDACTED]"),
        "the leaked secret must be redacted in the handshake error: {msg}"
    );
    assert!(
        !msg.contains(secret),
        "the raw secret value must not leak into the handshake error: {msg}"
    );
}

// --- Phase 2: adapter + discovery verbs ----------------------------------

use agenkitty::policy::ToolMode;
use agenkitty::tools::mcp::McpToolsInput;

/// A fixture config that allowlists the two fixture tools so they resolve to
/// `Allow` admission (and thus register). Without an allowlist an untrusted
/// server's tools are `Deny` and pre-filtered out of the registry (Phase 3).
fn allowlisted_fixture_config() -> McpServerConfig {
    let mut config = fixture_config(&[]);
    config.allowed_tools = vec!["alpha".to_string(), "beta".to_string()];
    config
}

/// A pooled runtime connected to the fixture server (tools allowlisted → Allow).
async fn connected_runtime(root: &std::path::Path) -> Arc<McpRuntime> {
    let runtime = Arc::new(
        McpRuntime::new(vec![allowlisted_fixture_config()])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();
    runtime
}

/// A connected, allowlisted runtime with the allowlisted tools' pins **approved**
/// — mirroring `register_mcp_tools` now that every dispatch mode (Allow included)
/// requires an approved current pin (C2).
async fn approved_runtime(root: &std::path::Path) -> Arc<McpRuntime> {
    let runtime = connected_runtime(root).await;
    let conn = runtime.connection("fixture").expect("fixture connected");
    for tool in conn.tools() {
        runtime
            .pins()
            .approve("fixture", &tool.name, &tool.pin_hash());
    }
    runtime
}

/// Build an `Allow` adapter (bound to a connected, pin-approved runtime) for a
/// fixture tool. The adapter resolves a live connection per calling principal at
/// dispatch (C1); the returned runtime keeps the pool alive for the test.
async fn adapter_for(
    root: &std::path::Path,
    tool_name: &'static str,
    id: &'static str,
) -> (Arc<McpRuntime>, McpToolAdapter) {
    let runtime = approved_runtime(root).await;
    let conn = runtime.connection("fixture").expect("fixture connected");
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == tool_name)
        .expect("fixture exposes the tool")
        .clone();
    let adapter = McpToolAdapter::new(
        id,
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();
    (runtime, adapter)
}

/// Like [`adapter_for`] but at an explicit `mode` and returning the shared
/// observer, so a test can assert the emitted events / audit entries. An `Allow`
/// adapter is pin-approved (mirroring registration, C2); other modes are left
/// unapproved so the pin gate is exercised.
async fn adapter_with_observer(
    root: &std::path::Path,
    tool_name: &'static str,
    id: &'static str,
    mode: ToolMode,
) -> (Arc<McpRuntime>, McpToolAdapter, Arc<McpObserver>) {
    let runtime = connected_runtime(root).await;
    let conn = runtime.connection("fixture").expect("fixture connected");
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == tool_name)
        .expect("fixture exposes the tool")
        .clone();
    if mode == ToolMode::Allow {
        runtime
            .pins()
            .approve("fixture", tool_name, &tool.pin_hash());
    }
    let observer = Arc::new(McpObserver::default());
    let adapter = McpToolAdapter::new(
        id,
        "fixture",
        &tool,
        runtime.clone(),
        mode,
        runtime.pins(),
        observer.clone(),
    )
    .unwrap();
    (runtime, adapter, observer)
}

#[tokio::test]
async fn remote_tools_register_as_namespaced_ids() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;

    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let builder = register_mcp_tools(builder, runtime.clone()).unwrap();
    let agenkit = builder.build().unwrap();

    // The two discovered fixture tools register under the dot-namespaced id.
    assert!(agenkit.tools().contains("mcp.fixture.alpha"));
    assert!(agenkit.tools().contains("mcp.fixture.beta"));
    // The discovery verbs are registered too.
    assert!(agenkit.tools().contains("mcp.servers"));
    assert!(agenkit.tools().contains("mcp.tools"));
}

#[tokio::test]
async fn tools_call_round_trips_the_fixture() {
    let (_dir, root) = workspace();
    let (_runtime, adapter) = adapter_for(&root, "alpha", "mcp.fixture.alpha").await;

    let out = adapter
        .invoke(
            serde_json::json!({ "text": "hello world" }),
            &Principal::anonymous(),
        )
        .await
        .expect("tools/call should round-trip");

    assert_eq!(out["content"][0]["type"], "text");
    assert_eq!(out["content"][0]["text"], "hello world");
    assert_eq!(out["structuredContent"]["text"], "hello world");
    assert_eq!(out["isError"], false);
    assert_eq!(out["elided"], false);
}

#[tokio::test]
async fn tool_execution_error_is_surfaced_recoverably() {
    let (_dir, root) = workspace();
    let (_runtime, adapter) = adapter_for(&root, "alpha", "mcp.fixture.alpha").await;

    // `isError: true` must come back as an `Ok` output carrying the flag, NOT a
    // protocol `Err` (SEP-1303) — the model reads it and self-corrects.
    let out = adapter
        .invoke(serde_json::json!({ "fail": true }), &Principal::anonymous())
        .await
        .expect("a tool-execution error is a recoverable result, not an Err");
    assert_eq!(out["isError"], true);
}

#[tokio::test]
async fn output_is_bounded_and_planted_secret_is_redacted() {
    use agenkitty::tools::secrets::{
        InMemorySecretResolver, SecretMetadata, SecretRequestInput, SecretRuntime, SecretScope,
    };

    let (_dir, root) = workspace();
    let secret = "supersecret-token-value";

    // Inject the secret as the server's per-server env so the *live connection*
    // carries it as its redaction set (the real D6/D9 path the adapter now reads
    // per call, rather than a synthetic redaction list).
    let resolver = InMemorySecretResolver::new().insert(
        SecretMetadata::new("redact-token", "redaction token", SecretScope::User)
            .with_purposes(["mcp-env"])
            .with_target_tools(["mcp.fixture"])
            .with_destinations(["MCP_SECRET"]),
        SecretString::new(secret),
    );
    let secret_runtime =
        Arc::new(SecretRuntime::new(Arc::new(resolver)).with_request_mode(ToolMode::Allow));
    let principal = Principal::anonymous();
    let grant = secret_runtime
        .request(
            SecretRequestInput {
                secret_ref: "redact-token".to_string(),
                purpose: "mcp-env".to_string(),
                target_tool: "mcp.fixture".to_string(),
                destination: Some("MCP_SECRET".to_string()),
                ttl_ms: Some(60_000),
                inheritable: None,
            },
            &principal,
        )
        .await
        .expect("grant should issue");

    let mut config = allowlisted_fixture_config();
    if let McpTransportConfig::Stdio { env, .. } = &mut config.transport {
        env.insert("MCP_SECRET".to_string(), grant.handle_id.clone());
    }
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_secret_runtime(secret_runtime),
    );
    runtime.connect_all_as(&principal).await.unwrap();
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    runtime.pins().approve("fixture", "alpha", &tool.pin_hash());
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    // A large echoed payload that embeds the resolved secret value.
    let big = format!("{}{}", secret, "A".repeat(64 * 1024));
    let out = adapter
        .invoke(serde_json::json!({ "text": big }), &principal)
        .await
        .expect("tools/call should round-trip");

    let serialized = serde_json::to_string(&out).unwrap();
    assert!(
        !serialized.contains(secret),
        "the planted secret must be redacted from the output"
    );
    assert!(
        serialized.contains("[REDACTED]"),
        "the redaction marker should be present"
    );
    assert_eq!(out["elided"], true, "the oversized text must be bounded");

    runtime.shutdown_all().await;
}

#[tokio::test]
async fn mcp_tools_lists_discovered_tools_without_secrets() {
    let (_dir, root) = workspace();
    // Allowlist the tools so they register/index under Phase 3 capability
    // admission. The published index is built from discovery and never carries
    // command/env/secret material (secret-handle env non-leak through the child
    // is proven by `secret_env_is_injected_only_into_the_target_child`).
    let config = allowlisted_fixture_config();
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.clone())
            .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let _builder = register_mcp_tools(builder, runtime.clone()).unwrap();

    let listing = McpToolsTool::new(runtime).run(McpToolsInput::default());
    let ids: Vec<&str> = listing.tools.iter().map(|t| t.id.as_str()).collect();
    assert!(ids.contains(&"mcp.fixture.alpha"));
    assert!(ids.contains(&"mcp.fixture.beta"));

    let alpha = listing
        .tools
        .iter()
        .find(|t| t.id == "mcp.fixture.alpha")
        .unwrap();
    assert_eq!(alpha.server, "fixture");
    assert_eq!(alpha.description.as_deref(), Some("first tool"));
    assert!(!alpha.schema_digest.is_empty());

    // No secret-handle / command / env material in the listing.
    let serialized = serde_json::to_string(&listing).unwrap();
    assert!(!serialized.contains("secret-handle-xyz"));
    assert!(!serialized.contains("TOKEN"));
}

#[tokio::test]
async fn child_is_reaped_on_close() {
    let (_dir, root) = workspace();
    let conn = McpConnection::connect(&fixture_config(&[]), &root, &host_path())
        .await
        .expect("connect should succeed");
    let pid = conn.pid();
    conn.close().await.unwrap();

    // On Linux, a reaped process leaves no `/proc/<pid>` entry (not even a
    // zombie, since `close` awaited `wait`). Elsewhere we just trust `close`.
    #[cfg(target_os = "linux")]
    {
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "child {pid} should be reaped after close"
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

/// Scan `/proc` for pids (live or zombie) whose argv contains `marker`.
#[cfg(target_os = "linux")]
fn marker_pids(marker: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline"))
            && String::from_utf8_lossy(&cmdline).contains(marker)
        {
            found.push(pid);
        }
    }
    found
}

/// H5: a FAILED handshake must reap the sandboxed child synchronously (kill +
/// wait + reap) before `connect` returns — never leave it as a zombie for a
/// best-effort background reaper. The child's pid is never handed back on the
/// failure path, so we tag its argv with a unique marker (the fixture ignores
/// unknown flags) and assert no process carrying it survives the failed connect.
///
/// Pre-fix the failure path only group-SIGKILLed the child on drop and deferred
/// the reap, leaving a zombie observable in `/proc` here (the single-threaded
/// `#[tokio::test]` runtime can't run its background reaper without an await).
#[cfg(target_os = "linux")]
#[tokio::test]
async fn failed_handshake_reaps_child_no_zombie() {
    let (_dir, root) = workspace();
    let marker = format!("h5-reap-marker-{}", std::process::id());
    // --fail-init: `initialize` returns a JSON-RPC error → the handshake fails.
    // --marker <m>: an inert tag we use to find the child pid in `/proc`.
    let config = fixture_config(&["--fail-init", "--marker", &marker]);
    let err = McpConnection::connect(&config, &root, &host_path())
        .await
        .err()
        .expect("a failed handshake must error");
    assert_eq!(err.kind(), "config");

    let leaked = marker_pids(&marker);
    assert!(
        leaked.is_empty(),
        "a failed handshake left un-reaped child pids (zombies): {leaked:?}"
    );
}

// --- Phase 3: policy, capability admission & trust ------------------------

use agenkitty::tools::secrets::{
    InMemorySecretResolver, SecretMetadata, SecretRequestInput, SecretRuntime, SecretScope,
};

/// An un-allowlisted tool on an untrusted server resolves to `Deny` and is
/// pre-filtered: it is neither registered as a callable adapter nor surfaced in
/// the published index (capability-scoped admission, D5).
#[tokio::test]
async fn untrusted_server_tools_are_denied_and_not_registered() {
    let (_dir, root) = workspace();
    // Default fixture config: untrusted, no allowlist → every tool is Deny.
    let runtime = Arc::new(
        McpRuntime::new(vec![fixture_config(&[])])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let builder = register_mcp_tools(builder, runtime.clone()).unwrap();
    let agenkit = builder.build().unwrap();

    // The remote tools are denied → not registered.
    assert!(!agenkit.tools().contains("mcp.fixture.alpha"));
    assert!(!agenkit.tools().contains("mcp.fixture.beta"));
    // …and not indexed for `mcp.tools` either.
    assert!(runtime.tool_index().is_empty());
    // The metadata verbs are still wired.
    assert!(agenkit.tools().contains("mcp.servers"));
    assert!(agenkit.tools().contains("mcp.tools"));
}

/// An `Ask` tool fails closed until the host approves its pinned definition; a
/// later definition change (rug pull / silent `tools/list_changed` swap) clears
/// the approval, re-denying it until re-approval (D8/T2).
#[tokio::test]
async fn ask_tool_is_denied_until_approved_then_rug_pull_redenies() {
    let (_dir, root) = workspace();
    // Untrusted fixture; the adapter is driven at `Ask` explicitly.
    let runtime = Arc::new(
        McpRuntime::new(vec![fixture_config(&[])])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .expect("fixture exposes alpha")
        .clone();
    let pins = runtime.pins();
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Ask,
        pins.clone(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    // Ask + unapproved → fail closed.
    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("an unapproved Ask tool must be denied");
    assert_eq!(err.kind(), "tool_policy");

    // The host approves this exact pinned definition → the call now proceeds.
    pins.approve("fixture", "alpha", &tool.pin_hash());
    let out = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect("an approved Ask tool runs");
    assert_eq!(out["content"][0]["text"], "hi");

    // Rug pull: the server serves a different definition for the same id. The
    // changed observation clears the approval (the swap is NOT silently
    // adopted), so the call is denied again pending re-approval.
    assert_eq!(
        pins.observe("fixture", "alpha", "rug-pulled-hash"),
        PinStatus::Changed
    );
    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("a rug-pulled tool must be re-denied until re-approval");
    assert_eq!(err.kind(), "tool_policy");

    runtime.shutdown_all().await;
}

/// An installed [`ToolApprover`] resolves an unapproved `Ask` tool interactively
/// (M1d): approval records the pin (no re-prompt on the next call); denial fails
/// the call with the operator's reason. Without an approver the same call fails
/// closed (covered above).
#[tokio::test]
async fn ask_tool_resolves_through_the_host_approver() {
    let (_dir, root) = workspace();
    let approver = Arc::new(ScriptedApprover::new(true));
    let runtime = Arc::new(
        McpRuntime::new(vec![fixture_config(&[])])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_approver(approver.clone()),
    );
    runtime.connect_all().await.unwrap();
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .expect("fixture exposes alpha")
        .clone();
    let pins = runtime.pins();
    let observer = Arc::new(McpObserver::default());
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Ask,
        pins.clone(),
        observer.clone(),
    )
    .unwrap();

    // First call: the approver is consulted, approves, and the pin is recorded.
    let out = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect("an operator-approved Ask tool runs");
    assert_eq!(out["content"][0]["text"], "hi");
    assert!(pins.is_approved("fixture", "alpha", &tool.pin_hash()));
    // The interactive approval lands in the audit trail like any other approval.
    assert!(
        observer
            .audit()
            .iter()
            .any(|entry| entry.kind == McpAuditKind::Approval),
        "an operator approval must be audited"
    );

    // Second call: already pinned — no new approval request.
    adapter
        .invoke(
            serde_json::json!({ "text": "again" }),
            &Principal::anonymous(),
        )
        .await
        .expect("a pinned tool runs without re-prompting");
    {
        let seen = approver.seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "approval is asked exactly once");
        assert_eq!(seen[0].tool_id, "mcp.fixture.alpha");
        assert!(
            seen[0]
                .detail
                .as_ref()
                .is_some_and(|detail| detail["transport"].as_str().unwrap_or("").contains("stdio")),
            "the approval detail must carry the T1 transport payload"
        );
    }

    runtime.shutdown_all().await;
}

/// A rug-pulled (definition-changed) adapter must NOT be resolvable through the
/// approver: the operator would be judging registration-time metadata while the
/// live tool differs. The approver is never consulted; the call stays fail
/// closed pending re-registration — the pin guard survives M1d intact.
#[tokio::test]
async fn rug_pulled_adapter_never_prompts_the_approver() {
    let (_dir, root) = workspace();
    let approver = Arc::new(ScriptedApprover::new(true));
    let runtime = Arc::new(
        McpRuntime::new(vec![fixture_config(&[])])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_approver(approver.clone()),
    );
    runtime.connect_all().await.unwrap();
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .expect("fixture exposes alpha")
        .clone();
    let pins = runtime.pins();
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Ask,
        pins.clone(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    // First call: normal interactive approval (observes + pins the live
    // definition).
    adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect("an operator-approved Ask tool runs");
    assert_eq!(approver.seen.lock().unwrap().len(), 1);

    // The server rug-pulls: the live observed definition no longer matches the
    // adapter's registration-time pin; the prior approval is cleared.
    assert_eq!(
        pins.observe("fixture", "alpha", "rug-pulled-hash"),
        PinStatus::Changed
    );

    // The approver is installed and says yes to everything — and must STILL
    // never be consulted: the operator would be judging registration-time
    // metadata while the live tool differs.
    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("a rug-pulled adapter must stay fail closed");
    assert_eq!(err.kind(), "tool_policy");
    assert_eq!(
        approver.seen.lock().unwrap().len(),
        1,
        "the approver must not be re-consulted for a stale definition"
    );
    assert!(
        !pins.is_approved("fixture", "alpha", &tool.pin_hash()),
        "no approval may be recorded for the stale registration-time hash"
    );

    runtime.shutdown_all().await;
}

/// A definition change landing while the operator is deciding (the human await
/// can take minutes) must refuse the approval: `approve_if_current` binds to the
/// exact prompted hash, so a mid-prompt swap fails closed instead of dispatching
/// under metadata the operator approved before the change.
#[tokio::test]
async fn definition_change_during_the_approval_prompt_refuses_the_approval() {
    use agenkitty::tools::mcp::PinStore;

    /// Approves everything — but swaps the observed definition first, modeling
    /// a `tools/list_changed` rediscovery landing mid-prompt.
    struct MidPromptSwapApprover {
        pins: Arc<PinStore>,
    }

    impl ToolApprover for MidPromptSwapApprover {
        fn approve<'a>(&'a self, _request: ApprovalRequest) -> BoxFuture<'a, ApprovalDecision> {
            Box::pin(async move {
                self.pins.observe("fixture", "alpha", "changed-mid-prompt");
                ApprovalDecision::Approved
            })
        }
    }

    let (_dir, root) = workspace();
    let runtime_seed = McpRuntime::new(vec![fixture_config(&[])])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path());
    let pins = runtime_seed.pins();
    let runtime = Arc::new(
        runtime_seed.with_approver(Arc::new(MidPromptSwapApprover { pins: pins.clone() })),
    );
    runtime.connect_all().await.unwrap();
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .expect("fixture exposes alpha")
        .clone();
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Ask,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("an approval for a definition that changed mid-prompt must be refused");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &tool.pin_hash()),
        "the stale prompted hash must not be approved"
    );
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", "changed-mid-prompt"),
        "the swapped definition must not be silently approved either"
    );

    runtime.shutdown_all().await;
}

/// Operator denial fails the call with the operator's reason and records no pin.
#[tokio::test]
async fn ask_tool_denied_by_the_host_approver_fails_closed() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![fixture_config(&[])])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_approver(Arc::new(ScriptedApprover::new(false))),
    );
    runtime.connect_all().await.unwrap();
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .expect("fixture exposes alpha")
        .clone();
    let pins = runtime.pins();
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Ask,
        pins.clone(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("an operator-denied Ask tool must fail");
    assert_eq!(err.kind(), "tool_policy");
    assert!(err.to_string().contains("denied by the operator"));
    assert!(!pins.is_approved("fixture", "alpha", &tool.pin_hash()));

    runtime.shutdown_all().await;
}

/// A per-server secret handle is resolved late and injected into the target
/// child's environment only — never into config, args, or the published index
/// (D6). A non-target var stays unset.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn secret_env_is_injected_only_into_the_target_child() {
    let (_dir, root) = workspace();

    let secret_value = "ghp_resolved_secret_value";
    let resolver = InMemorySecretResolver::new().insert(
        SecretMetadata::new("github-token", "GitHub token", SecretScope::User)
            .with_purposes(["mcp-env"])
            .with_target_tools(["mcp.fixture"])
            .with_destinations(["MCP_SECRET"]),
        SecretString::new(secret_value),
    );
    let secret_runtime =
        Arc::new(SecretRuntime::new(Arc::new(resolver)).with_request_mode(ToolMode::Allow));
    let principal = Principal::anonymous();
    let grant = secret_runtime
        .request(
            SecretRequestInput {
                secret_ref: "github-token".to_string(),
                purpose: "mcp-env".to_string(),
                target_tool: "mcp.fixture".to_string(),
                destination: Some("MCP_SECRET".to_string()),
                ttl_ms: Some(60_000),
                inheritable: None,
            },
            &principal,
        )
        .await
        .expect("grant should issue");

    // Config: reflect both the secret var and a never-granted var; map the env
    // var to the opaque handle id (never the value).
    let mut config = fixture_config(&["--reflect-env", "MCP_SECRET,OTHER_VAR"]);
    config.allowed_tools = vec!["alpha".to_string(), "beta".to_string()];
    if let McpTransportConfig::Stdio { env, .. } = &mut config.transport {
        env.insert("MCP_SECRET".to_string(), grant.handle_id.clone());
    }

    // The config carries only the handle id, never the resolved value.
    let config_json = serde_json::to_string(&config).unwrap();
    assert!(!config_json.contains(secret_value));
    assert!(config_json.contains(&grant.handle_id));

    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_secret_runtime(secret_runtime),
    );
    runtime.connect_all_as(&principal).await.unwrap();

    let conn = runtime.connection("fixture").expect("server is pooled");
    let instructions = conn.instructions().unwrap_or("").to_string();

    // The resolved secret reached the target child's env — the fixture read it
    // and reflected it back through `instructions`, where the harness then
    // redacted it (RC1, instructions are a model-visible discovery surface). The
    // [REDACTED] marker (vs `__unset__`) proves BOTH injection and redaction:
    // redaction only replaces the exact resolved value, so it can only appear if
    // the var was set to the secret.
    assert!(
        instructions.contains("env:MCP_SECRET=[REDACTED]"),
        "the resolved secret must be injected into the child env (then redacted): {instructions:?}"
    );
    assert!(
        !instructions.contains(secret_value),
        "the secret must be redacted from the model-visible instructions (RC1): {instructions:?}"
    );
    // …but a var with no grant is not set.
    assert!(
        instructions.contains("env:OTHER_VAR=__unset__"),
        "a non-granted var must stay unset: {instructions:?}"
    );

    // The published index never carries the secret value or the handle id.
    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let _builder = register_mcp_tools(builder, runtime.clone()).unwrap();
    let listing = format!("{:?}", runtime.tool_index());
    assert!(!listing.contains(secret_value));
    assert!(!listing.contains(&grant.handle_id));
}

/// Without a configured `SecretRuntime`, a server that declares secret-handle
/// env fails closed (the secret cannot be resolved, so the child is never
/// spawned with a partial/unresolved env).
#[tokio::test]
async fn secret_env_without_runtime_fails_closed() {
    let (_dir, root) = workspace();
    let mut config = allowlisted_fixture_config();
    if let McpTransportConfig::Stdio { env, .. } = &mut config.transport {
        env.insert("MCP_SECRET".to_string(), "some-handle".to_string());
    }
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path()),
    );
    let err = runtime
        .connect("fixture")
        .await
        .err()
        .expect("a secret-env server must fail closed without a SecretRuntime");
    assert_eq!(err.kind(), "tool_policy");
}

// --- Phase 5: resources & prompts -----------------------------------------

use agenkitty::tools::mcp::{
    McpGetPromptInput, McpGetPromptTool, McpReadResourceInput, McpReadResourceTool,
};

/// The Phase 5 verbs register alongside the discovery verbs.
#[tokio::test]
async fn resource_and_prompt_verbs_are_registered() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;

    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let builder = register_mcp_tools(builder, runtime).unwrap();
    let agenkit = builder.build().unwrap();

    assert!(agenkit.tools().contains("mcp.read_resource"));
    assert!(agenkit.tools().contains("mcp.get_prompt"));
}

/// `mcp.read_resource` round-trips a resource and bounds an oversized body
/// (the fixture returns 128 KiB for a uri containing `big`).
#[tokio::test]
async fn read_resource_round_trips_and_is_bounded() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;

    // A normal (small) read.
    let small = McpReadResourceTool::new(runtime.clone())
        .run(
            McpReadResourceInput {
                server: "fixture".to_string(),
                uri: "file:///doc.txt".to_string(),
            },
            &Principal::anonymous(),
        )
        .await
        .expect("resource read should succeed");
    assert_eq!(small.server, "fixture");
    assert_eq!(small.contents.len(), 1);
    assert!(!small.elided);
    assert!(
        small.contents[0]
            .text
            .as_deref()
            .unwrap()
            .contains("resource-contents-for file:///doc.txt")
    );

    // An oversized read is bounded + flagged.
    let big = McpReadResourceTool::new(runtime)
        .run(
            McpReadResourceInput {
                server: "fixture".to_string(),
                uri: "file:///big/doc.txt".to_string(),
            },
            &Principal::anonymous(),
        )
        .await
        .expect("resource read should succeed");
    let text = big.contents[0].text.as_ref().expect("text content");
    assert!(big.elided, "an oversized resource must be bounded");
    assert!(text.len() < 128 * 1024, "the body must be capped");
}

/// An out-of-allowlist uri scheme is refused *before* any request reaches the
/// server (the scheme check is at the verb edge).
#[tokio::test]
async fn read_resource_out_of_allowlist_uri_is_rejected() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;

    for uri in ["ftp://attacker/x", "javascript:alert(1)"] {
        let err = McpReadResourceTool::new(runtime.clone())
            .run(
                McpReadResourceInput {
                    server: "fixture".to_string(),
                    uri: uri.to_string(),
                },
                &Principal::anonymous(),
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("{uri} must be rejected"));
        assert_eq!(err.kind(), "tool_policy", "{uri}");
    }
}

/// `mcp.get_prompt` returns the (injection-shaped) prompt as **inert, untrusted
/// data** nested under `messages` — never promoted to an instruction channel.
#[tokio::test]
async fn get_prompt_returns_untrusted_template_never_an_instruction() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;

    let out = McpGetPromptTool::new(runtime)
        .run(
            McpGetPromptInput {
                server: "fixture".to_string(),
                name: "greet".to_string(),
                arguments: None,
            },
            &Principal::anonymous(),
        )
        .await
        .expect("prompt fetch should succeed");

    assert!(out.untrusted, "the prompt output must be flagged untrusted");
    assert_eq!(out.description.as_deref(), Some("fixture prompt"));
    assert_eq!(out.messages.len(), 1);
    assert_eq!(out.messages[0].role, "user");

    // The injection-shaped text is preserved AS DATA under messages[].content.
    let text = out.messages[0].content["text"]
        .as_str()
        .expect("text content");
    assert!(text.contains("Ignore all previous instructions"));
    assert!(text.contains("prompt=greet"));

    // Serialized, it nests under `messages` with no instruction/system channel.
    let json = serde_json::to_value(&out).unwrap();
    assert_eq!(json["untrusted"], true);
    assert!(json.get("system").is_none());
    assert!(json.get("instructions").is_none());
    assert_eq!(
        json["messages"][0]["content"]["text"].as_str().unwrap(),
        text
    );
}

// --- Phase 6: lifecycle & scale -------------------------------------------

use agenkitty::tools::mcp::{McpCallInput, McpCallTool, McpScope};

/// A fixture config under a given name + scope, with both tools allowlisted
/// (→ Allow, so they register and connect cleanly).
fn scoped_fixture_config(name: &str, scope: McpScope, args: &[&str]) -> McpServerConfig {
    let mut config = McpServerConfig {
        name: name.to_string(),
        transport: McpTransportConfig::Stdio {
            command: FIXTURE.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: BTreeMap::new(),
            cwd: None,
        },
        ..McpServerConfig::stdio(name, FIXTURE)
    };
    config.scope = scope;
    config.allowed_tools = vec!["alpha".to_string(), "beta".to_string()];
    config
}

/// A `session`-scoped server is torn down at session end; a `project`-scoped one
/// persists in the pool (re-derivation is config-driven, never a copied
/// transport).
#[tokio::test]
async fn session_scoped_server_is_torn_down_at_session_end() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![
            scoped_fixture_config("session_srv", McpScope::Session, &[]),
            scoped_fixture_config("project_srv", McpScope::Project, &[]),
        ])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();
    assert!(runtime.is_connected("session_srv"));
    assert!(runtime.is_connected("project_srv"));

    runtime.teardown_session().await;

    // The session server is gone; the project server persists.
    assert!(!runtime.is_connected("session_srv"));
    assert!(runtime.is_connected("project_srv"));

    runtime.shutdown_all().await;
    assert!(!runtime.is_connected("project_srv"));
}

/// A fork re-derives connections from config and copies **no** live transport:
/// the forked runtime starts disconnected and, when connected, spawns a fresh
/// child (a different pid) — proving nothing was carried over.
#[tokio::test]
async fn fork_re_derives_connections_without_copying_a_transport() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;
    assert!(runtime.is_connected("fixture"));
    let original_pid = runtime.connection("fixture").unwrap().pid();

    // The fork shares config but has an empty pool — no transport copied.
    let forked = runtime.fork();
    assert!(
        !forked.is_connected("fixture"),
        "a fork must not inherit a live connection"
    );

    // Re-deriving connects a brand-new child (different pid).
    forked.connect_all().await.unwrap();
    assert!(forked.is_connected("fixture"));
    let forked_pid = forked.connection("fixture").unwrap().pid();
    assert_ne!(
        original_pid, forked_pid,
        "the fork must spawn its own child, not reuse the source's"
    );
    // The source connection is still alive and independent.
    assert!(runtime.connection("fixture").unwrap().is_alive());

    runtime.shutdown_all().await;
    forked.shutdown_all().await;
}

/// After a connection is dropped/evicted, `reconnect` opens a fresh transport.
#[tokio::test]
async fn reconnect_after_drop_opens_a_fresh_connection() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;
    let pid1 = runtime.connection("fixture").unwrap().pid();

    // Simulate a dropped/dead transport: evict it from the pool.
    let evicted = runtime.evict("fixture").expect("server was pooled");
    assert!(!runtime.is_connected("fixture"));
    drop(evicted);

    let reconnected = runtime.reconnect("fixture").await.unwrap();
    assert!(runtime.is_connected("fixture"));
    assert!(reconnected.is_alive());
    assert_ne!(
        pid1,
        reconnected.pid(),
        "reconnect must spawn a fresh child after a drop"
    );

    runtime.shutdown_all().await;
}

/// A per-request timeout is enforced: a hanging `tools/call` is cancelled and
/// surfaced as a timeout error rather than blocking the agent loop forever.
#[tokio::test]
async fn per_request_timeout_is_enforced() {
    let (_dir, root) = workspace();
    let mut config = fixture_config(&[]);
    config.request_timeout_ms = 300; // tiny, so the hang trips it fast
    let conn = McpConnection::connect(&config, &root, &host_path())
        .await
        .expect("connect should succeed");

    let args = serde_json::json!({ "hang": true }).as_object().cloned();
    let err = conn
        .call_tool("alpha", args)
        .await
        .expect_err("a hanging call must time out");
    assert_eq!(err.kind(), "internal");
    assert!(
        err.to_string().contains("timed out"),
        "the error should name the timeout: {err}"
    );

    conn.close().await.unwrap();
}

/// A `tools/list_changed` re-discovery re-pin-checks the result: a server that
/// swaps a definition produces a *changed* pin that clears the prior approval —
/// the swap is never silently adopted (T2).
#[tokio::test]
async fn list_changed_triggers_re_pin_not_silent_adoption() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--mutate-relist"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    // Approve alpha at its originally-discovered pin.
    let conn = runtime.connection("fixture").unwrap();
    let original_hash = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);
    assert!(
        runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash)
    );

    // Re-discover (as on a tools/list_changed): the fixture now serves a mutated
    // alpha description → a Changed pin → the approval is cleared.
    let outcomes = runtime.rediscover("fixture").await.unwrap();
    let alpha_status = outcomes
        .iter()
        .find(|(name, _)| name == "alpha")
        .map(|(_, status)| *status)
        .expect("alpha re-observed");
    assert_eq!(alpha_status, PinStatus::Changed);
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "a changed definition must clear the prior approval (not silently adopted)"
    );

    runtime.shutdown_all().await;
}

/// The `mcp.call` escape hatch runs an allowlisted (Allow) tool and refuses a
/// denied one — going through the same admission gate as the adapter.
#[tokio::test]
async fn mcp_call_escape_hatch_respects_admission() {
    let (_dir, root) = workspace();

    // Allowlisted (Allow) + an approved pin (mirrors registration, required now
    // that every dispatch mode checks the pin, C2) → mcp.call runs.
    let allowed = approved_runtime(&root).await;
    let out = McpCallTool::new(allowed.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "via mcp.call" })
                    .as_object()
                    .cloned(),
            },
            &Principal::anonymous(),
        )
        .await
        .expect("an allowed tool runs through mcp.call");
    assert_eq!(out.result["content"][0]["text"], "via mcp.call");
    assert_eq!(out.result["isError"], false);

    // Untrusted, un-allowlisted (Deny) → mcp.call refuses with tool_policy.
    let denied = Arc::new(
        McpRuntime::new(vec![fixture_config(&[])])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path()),
    );
    denied.connect_all().await.unwrap();
    let err = McpCallTool::new(denied.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: None,
            },
            &Principal::anonymous(),
        )
        .await
        .expect_err("a denied tool must be refused by mcp.call");
    assert_eq!(err.kind(), "tool_policy");

    allowed.shutdown_all().await;
    denied.shutdown_all().await;
}

/// Deferred schema loading (D12): the default `mcp.tools` listing carries no
/// full input schema (no prompt pre-bloat); the schema is fetched only on
/// demand via `include_schema: true`.
#[tokio::test]
async fn deferred_schema_loading_does_not_pre_bloat() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;
    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let _builder = register_mcp_tools(builder, runtime.clone()).unwrap();

    // Default listing: lean (no schema) — the deferred-loading contract.
    let lean = McpToolsTool::new(runtime.clone()).run(McpToolsInput::default());
    assert!(lean.tools.iter().all(|t| t.input_schema.is_none()));
    let lean_json = serde_json::to_string(&lean).unwrap();
    assert!(!lean_json.contains("inputSchema"));
    assert!(!lean_json.contains("input_schema"));

    // On demand: the full schema is included for the requested server.
    let detailed = McpToolsTool::new(runtime.clone()).run(McpToolsInput {
        server: Some("fixture".to_string()),
        include_schema: true,
    });
    assert!(
        detailed.tools.iter().all(|t| t.input_schema.is_some()),
        "include_schema must attach the full schema on demand"
    );
    // H2: the surfaced schema (here `beta` has a poisoned nested property
    // description) is sanitized end-to-end — no ANSI/control byte reaches the
    // model through `mcp.tools include_schema`.
    let detailed_json = serde_json::to_string(&detailed).unwrap();
    assert!(
        !detailed_json.contains('\u{1b}'),
        "a nested schema description must not smuggle ANSI into include_schema: {detailed_json}"
    );

    runtime.shutdown_all().await;
}

// --- Phase 7: tracing, eventing, audit ------------------------------------

#[tokio::test]
async fn connect_emits_framework_event_and_audit() {
    use agenkitty::events::FrameworkEventKind;
    use agenkitty::tools::mcp::McpAuditKind;

    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await;
    // Registration is where allowlisted tools are auto-approved (the approval
    // audit) and is the realistic host flow; connect already happened above.
    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    register_mcp_tools(builder, runtime.clone()).unwrap();

    // A connect must have produced a ToolStarted + ToolCompleted pair and a
    // matching Connect audit row.
    let kinds: Vec<_> = runtime.events().into_iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&FrameworkEventKind::ToolStarted));
    assert!(kinds.contains(&FrameworkEventKind::ToolCompleted));

    let audit = runtime.audit();
    assert!(
        audit
            .iter()
            .any(|entry| entry.kind == McpAuditKind::Connect && entry.server == "fixture"),
        "a connect audit entry must be present"
    );
    // Allowlisted tools are auto-approved at registration → an approval audit row.
    assert!(
        audit
            .iter()
            .any(|entry| entry.kind == McpAuditKind::Approval),
        "an approval audit entry must be present for an allowlisted tool"
    );

    runtime.shutdown_all().await;
}

#[tokio::test]
async fn successful_call_emits_started_and_completed_events() {
    use agenkitty::events::FrameworkEventKind;

    let (_dir, root) = workspace();
    let (_runtime, adapter, observer) =
        adapter_with_observer(&root, "alpha", "mcp.fixture.alpha", ToolMode::Allow).await;

    adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect("an Allow tool runs");

    let kinds: Vec<_> = observer.events().into_iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            FrameworkEventKind::ToolStarted,
            FrameworkEventKind::ToolCompleted,
        ]
    );
}

#[tokio::test]
async fn blocked_call_emits_tool_blocked_event() {
    use agenkitty::events::FrameworkEventKind;
    use agenkitty::tools::mcp::McpAuditOutcome;

    let (_dir, root) = workspace();
    // An Ask tool with no approval fails closed → the call is blocked by policy.
    let (_runtime, adapter, observer) =
        adapter_with_observer(&root, "alpha", "mcp.fixture.alpha", ToolMode::Ask).await;

    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("an unapproved Ask tool is blocked");
    assert_eq!(err.kind(), "tool_policy");

    let events = observer.events();
    assert_eq!(events[0].kind, FrameworkEventKind::ToolStarted);
    assert_eq!(events[1].kind, FrameworkEventKind::ToolBlocked);

    let audit = observer.audit();
    assert!(
        audit
            .iter()
            .any(|entry| entry.outcome == McpAuditOutcome::Blocked),
        "a blocked call must record a Blocked audit outcome"
    );
}

/// M4: dispatching an unapproved `Ask` tool surfaces a T1 approval request that
/// names the concrete connection target (the stdio command), so a host UI has the
/// data to render the prompt — while the call still fails closed.
#[tokio::test]
async fn unapproved_ask_dispatch_surfaces_an_approval_request_with_the_command() {
    use agenkitty::tools::mcp::McpAuditKind;

    let (_dir, root) = workspace();
    let (_runtime, adapter, observer) =
        adapter_with_observer(&root, "alpha", "mcp.fixture.alpha", ToolMode::Ask).await;

    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("an unapproved Ask tool is denied");
    assert_eq!(err.kind(), "tool_policy");

    // The approval request is recorded, carrying the exact spawn command (the
    // fixture path) so the host can satisfy T1.
    let request = observer
        .audit()
        .into_iter()
        .find(|entry| entry.kind == McpAuditKind::ApprovalRequested)
        .expect("an unapproved Ask dispatch must record an ApprovalRequested audit row");
    assert_eq!(request.tool.as_deref(), Some("alpha"));
    let detail = request.detail.unwrap_or_default();
    assert!(
        detail.contains("stdio:") && detail.contains(FIXTURE),
        "the approval request must name the stdio spawn command: {detail:?}"
    );
}

#[tokio::test]
async fn no_secret_leaks_into_events_or_audit() {
    let (_dir, root) = workspace();
    let secret = "supersecret-token-value";
    // A failing call whose error path could carry text — the secret value must
    // never surface in any recorded event or audit payload.
    let (_runtime, adapter, observer) =
        adapter_with_observer(&root, "alpha", "mcp.fixture.alpha", ToolMode::Allow).await;

    let _ = adapter
        .invoke(
            serde_json::json!({ "text": format!("{secret} body") }),
            &Principal::anonymous(),
        )
        .await;

    let events_json = serde_json::to_string(&observer.events()).unwrap();
    let audit_json = serde_json::to_string(&observer.audit()).unwrap();
    assert!(!events_json.contains(secret), "no secret in events");
    assert!(!audit_json.contains(secret), "no secret in audit");
}

#[tokio::test]
async fn config_round_trips_from_mcp_servers_json() {
    let json = r#"{
        "mcpServers": {
            "github": {
                "command": "github-mcp",
                "args": ["stdio"],
                "trusted": true,
                "allowedTools": ["search"]
            },
            "remote": {
                "type": "http",
                "url": "https://mcp.example.com/"
            }
        }
    }"#;
    let runtime = McpRuntime::from_mcp_servers_json(json).unwrap();
    assert_eq!(runtime.servers().len(), 2);
    assert!(runtime.server("github").is_some());
    assert_eq!(runtime.server("github").unwrap().transport.kind(), "stdio");
    assert_eq!(runtime.server("remote").unwrap().transport.kind(), "http");
}

// --- Codex security fixes — Group A ---------------------------------------

/// C1 — a secret-bearing connection is **never** shared across principals. The
/// pool is keyed by `(server, principal)` and the imported-tool adapter resolves
/// a connection for the *calling* principal, so principal A's injected secret is
/// unreachable by principal B (who fails closed lacking A's grant) — across both
/// the runtime pool and the adapter dispatch path.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn secret_bearing_connection_is_isolated_per_principal() {
    let (_dir, root) = workspace();
    let secret = "ghp_alice_only_secret_value";

    // Alice holds a grant for the server's secret env; Bob does not.
    let resolver = InMemorySecretResolver::new().insert(
        SecretMetadata::new("alice-token", "Alice token", SecretScope::User)
            .with_purposes(["mcp-env"])
            .with_target_tools(["mcp.fixture"])
            .with_destinations(["MCP_SECRET"]),
        SecretString::new(secret),
    );
    let secret_runtime =
        Arc::new(SecretRuntime::new(Arc::new(resolver)).with_request_mode(ToolMode::Allow));
    let alice = Principal::from_user(AuthUser::new("alice"));
    let bob = Principal::from_user(AuthUser::new("bob"));
    let grant = secret_runtime
        .request(
            SecretRequestInput {
                secret_ref: "alice-token".to_string(),
                purpose: "mcp-env".to_string(),
                target_tool: "mcp.fixture".to_string(),
                destination: Some("MCP_SECRET".to_string()),
                ttl_ms: Some(60_000),
                inheritable: None,
            },
            &alice,
        )
        .await
        .expect("alice's grant should issue");

    let mut config = fixture_config(&["--reflect-env", "MCP_SECRET"]);
    config.allowed_tools = vec!["alpha".to_string(), "beta".to_string()];
    if let McpTransportConfig::Stdio { env, .. } = &mut config.transport {
        env.insert("MCP_SECRET".to_string(), grant.handle_id.clone());
    }
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_secret_runtime(secret_runtime),
    );

    // --- runtime pool ---
    // Alice connects → her child carries the secret.
    let alice_conn = runtime
        .ensure_connected_as("fixture", &alice)
        .await
        .expect("alice connects");
    // Alice's child carries her resolved secret — reflected back through
    // `instructions`, where the harness redacts it (RC1). The [REDACTED] marker
    // (vs `__unset__`) proves the value was set to her secret; the raw value
    // never appears in the model-visible instructions.
    assert!(
        alice_conn
            .instructions()
            .unwrap_or("")
            .contains("env:MCP_SECRET=[REDACTED]"),
        "alice's child must carry her resolved secret (redacted in instructions)"
    );
    assert!(
        !alice_conn.instructions().unwrap_or("").contains(secret),
        "the secret must be redacted from alice's model-visible instructions (RC1)"
    );

    // Bob must NOT be lent Alice's pooled, secret-bearing child: keyed by
    // (server, principal), his lookup misses Alice's, and his own connect fails
    // closed (no grant for the handle) — the secret is unreachable by Bob.
    let bob_result = runtime.ensure_connected_as("fixture", &bob).await;
    assert!(
        bob_result.is_err(),
        "Bob must not receive Alice's secret-bearing connection"
    );
    assert!(
        runtime.connection_as("fixture", &bob).is_none(),
        "no Bob-scoped connection is pooled"
    );
    let pooled_alice = runtime
        .connection_as("fixture", &alice)
        .expect("alice's connection is pooled");
    assert!(
        Arc::ptr_eq(&alice_conn, &pooled_alice),
        "alice's pooled connection is her own, independent handle"
    );

    // --- adapter dispatch path ---
    // Build an Allow adapter (pin approved). Alice's invoke runs against her
    // own secret-bearing child; Bob's invoke fails closed — the adapter never
    // reuses Alice's connection for Bob (it resolves per calling principal, C1).
    let tool = alice_conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    runtime.pins().approve("fixture", "alpha", &tool.pin_hash());
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    let alice_out = adapter
        .invoke(serde_json::json!({ "text": "ok" }), &alice)
        .await
        .expect("alice's call runs against her own connection");
    assert_eq!(alice_out["content"][0]["text"], "ok");

    let bob_err = adapter
        .invoke(serde_json::json!({ "text": "ok" }), &bob)
        .await
        .expect_err("Bob's call must fail closed, not ride Alice's connection");
    assert!(
        runtime.connection_as("fixture", &bob).is_none(),
        "Bob still has no pooled connection after a failed dispatch"
    );
    let _ = bob_err;

    runtime.shutdown_all().await;
}

/// C2 — an allowlisted (`Allow`) tool that rug-pulls is re-denied on the
/// **adapter** dispatch path: every dispatch mode now requires an approved
/// current pin, so a changed definition (cleared approval) fails closed even for
/// an allowlisted tool.
#[tokio::test]
async fn allowlisted_rug_pull_is_re_denied_on_adapter_dispatch() {
    let (_dir, root) = workspace();
    let runtime = approved_runtime(&root).await; // allowlisted alpha/beta, pins approved
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    // Allowlisted + approved → runs.
    let out = adapter
        .invoke(serde_json::json!({ "text": "ok" }), &Principal::anonymous())
        .await
        .expect("an approved allowlisted tool runs");
    assert_eq!(out["content"][0]["text"], "ok");

    // Rug pull: the server serves a changed definition for the same id; the
    // changed observation clears the approval.
    assert_eq!(
        runtime
            .pins()
            .observe("fixture", "alpha", "rug-pulled-hash"),
        PinStatus::Changed
    );

    // Even an allowlisted (Allow) tool is now RE-DENIED (Allow no longer skips
    // the pin check).
    let err = adapter
        .invoke(serde_json::json!({ "text": "ok" }), &Principal::anonymous())
        .await
        .expect_err("an allowlisted rug-pull must be re-denied");
    assert_eq!(err.kind(), "tool_policy");

    runtime.shutdown_all().await;
}

/// C2 — the same allowlisted rug-pull is re-denied through the `mcp.call` escape
/// hatch (its `Allow` branch also requires an approved current pin).
#[tokio::test]
async fn allowlisted_rug_pull_is_re_denied_on_mcp_call() {
    let (_dir, root) = workspace();
    let runtime = connected_runtime(&root).await; // allowlisted alpha/beta
    // Registration auto-approves the allowlisted tools at their discovered pins.
    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    register_mcp_tools(builder, runtime.clone()).unwrap();

    // First call succeeds (approved).
    let out = McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "ok" }).as_object().cloned(),
            },
            &Principal::anonymous(),
        )
        .await
        .expect("an approved allowlisted tool runs via mcp.call");
    assert_eq!(out.result["content"][0]["text"], "ok");

    // Rug pull: change the pinned definition for alpha.
    assert_eq!(
        runtime
            .pins()
            .observe("fixture", "alpha", "rug-pulled-hash"),
        PinStatus::Changed
    );

    // mcp.call now re-denies the allowlisted (Allow) tool.
    let err = McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: None,
            },
            &Principal::anonymous(),
        )
        .await
        .expect_err("an allowlisted rug-pull must be re-denied via mcp.call");
    assert_eq!(err.kind(), "tool_policy");

    runtime.shutdown_all().await;
}

/// H1 — a `tools/list_changed` notification invalidates the prior approval on the
/// **next dispatch, without any manual `runtime.rediscover` call**. Marking the
/// connection dirty (exactly what the client handler does on the notification)
/// triggers a re-discover + re-pin before dispatch; the fixture serves a mutated
/// `alpha` on the re-list (rug pull), so the approval is cleared and the call is
/// re-denied (combined with C2).
#[tokio::test]
async fn list_changed_notification_re_denies_without_manual_rediscover() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--mutate-relist"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();
    // Registration auto-approves the allowlisted alpha at its original pin.
    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    register_mcp_tools(builder, runtime.clone()).unwrap();

    let conn = runtime.connection("fixture").unwrap();
    let original_hash = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .pin_hash();
    assert!(
        runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "registration approves the allowlisted tool"
    );

    // Simulate the server announcing a tools/list_changed (what the handler does
    // on the notification): mark the connection dirty. NO manual rediscover call.
    conn.mark_list_changed();

    // The next dispatch must re-discover + re-pin BEFORE calling, detect the
    // mutated alpha (Changed), clear the approval, and re-deny.
    let err = McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "hi" }).as_object().cloned(),
            },
            &Principal::anonymous(),
        )
        .await
        .expect_err("a notification-triggered mutation must invalidate the prior approval");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "the prior approval must be invalidated by the notification-triggered re-pin"
    );

    runtime.shutdown_all().await;
}

// --- Codex re-review fixes — Group E --------------------------------------

/// RH1 — a `tools/list_changed` that arrives between approval and the next
/// **adapter** dispatch must re-deny the call. The prior ordering checked the pin
/// FIRST, then `ready_connection_as` rediscovered + revoked the approval, then
/// `call_tool` ran anyway under the now-stale approval (TOCTOU). The fix
/// rediscovers BEFORE the admission decision, so marking the connection dirty
/// (exactly what the handler does on the notification) re-denies the very next
/// adapter dispatch — with NO manual `runtime.rediscover` call.
#[tokio::test]
async fn list_changed_re_denies_on_next_adapter_dispatch_without_manual_rediscover() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--mutate-relist"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    let original_hash = tool.pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);

    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    // Approved → runs.
    let out = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect("an approved tool runs");
    assert_eq!(out["content"][0]["text"], "hi");

    // The server announces tools/list_changed (the handler marks the flag). NO
    // manual rediscover call.
    conn.mark_list_changed();

    // The very next adapter dispatch rediscovers (the fixture serves a MUTATED
    // alpha → a Changed pin → the approval is cleared) BEFORE deciding admission,
    // and is re-denied — never invoked under the stale approval (RH1).
    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("a notification-dirtied changed def must re-deny the next adapter dispatch");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "the notification-triggered re-pin must clear the prior approval"
    );

    runtime.shutdown_all().await;
}

/// RH2 — two concurrent dispatches racing a `tools/list_changed` mutation: NEITHER
/// may be admitted under the stale (pre-mutation) approval. Pre-fix, the dirty
/// flag was cleared (read+clear) before the async rediscovery completed and there
/// was no per-connection rediscovery lock, so one dispatch cleared the flag and
/// rediscovered while the other observed a clean flag, skipped rediscovery, and
/// (combined with the RH1 pin-first ordering) dispatched under the stale approval.
/// The per-`(server, principal)` rediscovery lock serializes the re-pin and the
/// RH1 reorder re-checks admission after it, so both dispatches are re-denied.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_dispatch_across_list_changed_denies_both() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--mutate-relist"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    let original_hash = tool.pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);
    assert!(
        runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash)
    );

    // One Allow adapter shared by both concurrent dispatchers.
    let adapter = Arc::new(
        McpToolAdapter::new(
            "mcp.fixture.alpha",
            "fixture",
            &tool,
            runtime.clone(),
            ToolMode::Allow,
            runtime.pins(),
            Arc::new(McpObserver::default()),
        )
        .unwrap(),
    );

    // The server announced a tools/list_changed (the handler marks the flag); the
    // re-list serves a MUTATED alpha (rug pull). Two dispatches now race the
    // rediscovery.
    conn.mark_list_changed();

    let a = {
        let adapter = adapter.clone();
        tokio::spawn(async move {
            adapter
                .invoke(serde_json::json!({ "text": "a" }), &Principal::anonymous())
                .await
        })
    };
    let b = {
        let adapter = adapter.clone();
        tokio::spawn(async move {
            adapter
                .invoke(serde_json::json!({ "text": "b" }), &Principal::anonymous())
                .await
        })
    };
    let (ra, rb) = tokio::join!(a, b);
    let ra = ra.expect("task a joined");
    let rb = rb.expect("task b joined");

    assert!(
        ra.is_err() && rb.is_err(),
        "neither concurrent dispatch may run under the stale approval: a={ra:?} b={rb:?}"
    );
    assert_eq!(ra.unwrap_err().kind(), "tool_policy");
    assert_eq!(rb.unwrap_err().kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "the mutated definition must clear the prior approval"
    );

    runtime.shutdown_all().await;
}

/// RM1 — a secret accidentally placed in a stdio server's argv must not surface in
/// the audit detail of an `Ask` approval request. The summary surfaces the command
/// path (operator-configured, useful) but never the raw argv, so a secret-shaped
/// arg that the observer's substring heuristic would NOT catch still can't leak.
/// The full args remain in the approval-payload DATA (a host UI needs them).
#[tokio::test]
async fn ask_approval_request_audit_does_not_leak_raw_stdio_args() {
    use agenkitty::tools::mcp::McpAuditKind;

    let (_dir, root) = workspace();
    // A secret-shaped arg deliberately NOT matched by the observer's redaction
    // markers (no "secret"/"token="/etc. substring), so the test proves the fix
    // doesn't rely on that heuristic. The fixture ignores unknown flags.
    let secret_arg = "sk-deadbeefcafe1234567890";
    let config = fixture_config(&["--ignored-flag", secret_arg]);
    // Untrusted fixture; drive the adapter at Ask (unapproved → approval request).
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();
    let conn = runtime.connection("fixture").unwrap();
    let tool = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    let observer = Arc::new(McpObserver::default());
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Ask,
        runtime.pins(),
        observer.clone(),
    )
    .unwrap();

    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &Principal::anonymous())
        .await
        .expect_err("an unapproved Ask tool is denied");
    assert_eq!(err.kind(), "tool_policy");

    let request = observer
        .audit()
        .into_iter()
        .find(|entry| entry.kind == McpAuditKind::ApprovalRequested)
        .expect("an unapproved Ask dispatch records an ApprovalRequested audit row");
    let detail = request.detail.unwrap_or_default();
    // The command path is still surfaced…
    assert!(
        detail.contains(FIXTURE),
        "the command path must be surfaced in the audit detail: {detail:?}"
    );
    // …but the raw argv is NOT — a secret accidentally placed in args can't leak.
    assert!(
        !detail.contains(secret_arg),
        "raw stdio args must not appear in the audit detail (RM1): {detail:?}"
    );

    runtime.shutdown_all().await;
}

// --- Codex re-review fixes — Group D --------------------------------------

/// RC1 — a server handed an injected secret env can echo it back through its
/// discovered tool **definitions** (a description and a nested inputSchema
/// string). The harness must redact the resolved secret out of the stored /
/// exposed / pinned def — it is never enough to redact only tool-CALL outputs.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn discovery_definitions_redact_injected_server_secret() {
    let (_dir, root) = workspace();
    let secret = "ghp_discovery_leak_secret_value";

    // Inject the secret as the server's per-server env (resolved from a handle),
    // so the live connection carries it as its redaction set — exactly the D6
    // path. The fixture (`--leak-env-tool`) then echoes that value into alpha's
    // description AND a nested schema string at discovery.
    let resolver = InMemorySecretResolver::new().insert(
        SecretMetadata::new("leak-token", "leak token", SecretScope::User)
            .with_purposes(["mcp-env"])
            .with_target_tools(["mcp.fixture"])
            .with_destinations(["MCP_SECRET"]),
        SecretString::new(secret),
    );
    let secret_runtime =
        Arc::new(SecretRuntime::new(Arc::new(resolver)).with_request_mode(ToolMode::Allow));
    let principal = Principal::anonymous();
    let grant = secret_runtime
        .request(
            SecretRequestInput {
                secret_ref: "leak-token".to_string(),
                purpose: "mcp-env".to_string(),
                target_tool: "mcp.fixture".to_string(),
                destination: Some("MCP_SECRET".to_string()),
                ttl_ms: Some(60_000),
                inheritable: None,
            },
            &principal,
        )
        .await
        .expect("grant should issue");

    let mut config = fixture_config(&["--leak-env-tool", "MCP_SECRET"]);
    config.allowed_tools = vec!["alpha".to_string(), "beta".to_string()];
    if let McpTransportConfig::Stdio { env, .. } = &mut config.transport {
        env.insert("MCP_SECRET".to_string(), grant.handle_id.clone());
    }
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_secret_runtime(secret_runtime),
    );
    runtime.connect_all_as(&principal).await.unwrap();

    // The stored def (the source of every model-facing surface) is redacted.
    let conn = runtime.connection("fixture").expect("fixture connected");
    let alpha = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .expect("alpha discovered")
        .clone();
    let desc = alpha.description.as_deref().unwrap_or("");
    assert!(
        !desc.contains(secret),
        "the discovered description leaked the secret: {desc}"
    );
    assert!(desc.contains("[REDACTED]"), "description redacted: {desc}");
    let schema_json = serde_json::to_string(&alpha.input_schema).unwrap();
    assert!(
        !schema_json.contains(secret),
        "the nested schema string leaked the secret: {schema_json}"
    );
    assert!(schema_json.contains("[REDACTED]"));

    // The pin binds the redacted form (pinned == model-visible): re-deriving the
    // pin from the stored fields reproduces it, and the stored fields are clean.
    let pin = alpha.pin_hash();
    assert!(!pin.is_empty());

    // The model-facing exposure path — `mcp.tools include_schema` — is clean too.
    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let _builder = register_mcp_tools(builder, runtime.clone()).unwrap();
    let listing = McpToolsTool::new(runtime.clone()).run(McpToolsInput {
        server: Some("fixture".to_string()),
        include_schema: true,
    });
    let listing_json = serde_json::to_string(&listing).unwrap();
    assert!(
        !listing_json.contains(secret),
        "mcp.tools must not leak the secret through the exposed def: {listing_json}"
    );
    assert!(listing_json.contains("[REDACTED]"));

    runtime.shutdown_all().await;
}

/// RH3 — a tool whose inputSchema carries a poisoned property KEY (ANSI/control
/// bytes the value-only schema sanitizer can't neutralize) is rejected at
/// discovery: it never registers as an adapter, never appears in `mcp.tools`,
/// and no escape byte reaches the model — while the clean tool still registers.
#[tokio::test]
async fn tool_with_a_poisoned_schema_key_is_rejected_and_never_surfaced() {
    let (_dir, root) = workspace();
    // `--poison-key` laces beta's schema with a control/ANSI property key; alpha
    // stays clean. Both are allowlisted so admission isn't the thing dropping beta.
    let mut config = fixture_config(&["--poison-key"]);
    config.allowed_tools = vec!["alpha".to_string(), "beta".to_string()];
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    // beta is dropped at discovery (unsafe key); only alpha survives in the snapshot.
    let conn = runtime.connection("fixture").expect("fixture connected");
    let names: Vec<&str> = conn.tools().iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["alpha"], "the poisoned-key tool must be dropped");

    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let builder = register_mcp_tools(builder, runtime.clone()).unwrap();
    let agenkit = builder.build().unwrap();

    // The clean tool registers; the poisoned one never reaches the registry.
    assert!(agenkit.tools().contains("mcp.fixture.alpha"));
    assert!(
        !agenkit.tools().contains("mcp.fixture.beta"),
        "a tool with an unsafe schema key must not register"
    );

    // …nor the published index / `mcp.tools` (including on-demand schemas), and
    // no escape byte reaches the model.
    let listing = McpToolsTool::new(runtime.clone()).run(McpToolsInput {
        server: Some("fixture".to_string()),
        include_schema: true,
    });
    let ids: Vec<&str> = listing.tools.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["mcp.fixture.alpha"]);
    let listing_json = serde_json::to_string(&listing).unwrap();
    assert!(
        !listing_json.contains('\u{1b}'),
        "no ESC byte may reach the model through mcp.tools: {listing_json}"
    );

    runtime.shutdown_all().await;
}

/// RC1b — a hostile server that NAMES a tool with its injected per-server secret
/// value is rejected at discovery: the secret-bearing name never mints an id,
/// never appears in `mcp.tools`, and the secret never reaches any model-facing
/// surface — while the clean tool still registers. The name is the dispatch
/// contract (can't be redacted in place), so the whole tool is dropped.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn tool_named_with_injected_secret_is_rejected_and_never_surfaced() {
    let (_dir, root) = workspace();
    // Charset-clean so the SECRET check (not the `[A-Za-z0-9_.-]` contract check)
    // is what drops the tool — exactly the RC1b leak-via-name path.
    let secret = "ghp_named_in_tool_secret_value";

    // Inject the secret as the server's per-server env (resolved from a handle),
    // so the live connection carries it as its redaction set — the same D6 path as
    // the description/schema leak test. `--name-env-tool` then NAMES alpha with it.
    let resolver = InMemorySecretResolver::new().insert(
        SecretMetadata::new("name-token", "name token", SecretScope::User)
            .with_purposes(["mcp-env"])
            .with_target_tools(["mcp.fixture"])
            .with_destinations(["MCP_SECRET"]),
        SecretString::new(secret),
    );
    let secret_runtime =
        Arc::new(SecretRuntime::new(Arc::new(resolver)).with_request_mode(ToolMode::Allow));
    let principal = Principal::anonymous();
    let grant = secret_runtime
        .request(
            SecretRequestInput {
                secret_ref: "name-token".to_string(),
                purpose: "mcp-env".to_string(),
                target_tool: "mcp.fixture".to_string(),
                destination: Some("MCP_SECRET".to_string()),
                ttl_ms: Some(60_000),
                inheritable: None,
            },
            &principal,
        )
        .await
        .expect("grant should issue");

    let mut config = fixture_config(&["--name-env-tool", "MCP_SECRET"]);
    // Allowlist the secret-name AND beta so ADMISSION isn't what drops the tool —
    // proving the DISCOVERY-time name rejection (RC1b) is.
    config.allowed_tools = vec![secret.to_string(), "beta".to_string()];
    if let McpTransportConfig::Stdio { env, .. } = &mut config.transport {
        env.insert("MCP_SECRET".to_string(), grant.handle_id.clone());
    }
    let runtime = Arc::new(
        McpRuntime::new(vec![config])
            .unwrap()
            .with_root(root.to_path_buf())
            .with_default_path(host_path())
            .with_secret_runtime(secret_runtime),
    );
    runtime.connect_all_as(&principal).await.unwrap();

    // The secret-named tool is dropped at discovery; the clean `beta` survives.
    let conn = runtime.connection("fixture").expect("fixture connected");
    let names: Vec<String> = conn.tools().iter().map(|t| t.name.clone()).collect();
    assert!(
        !names.iter().any(|n| n.contains(secret)),
        "the secret-named tool must be dropped at discovery: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "beta"),
        "the clean tool still discovers: {names:?}"
    );

    // No id minted from the secret name (the natural id would be a verbatim
    // sanitize of the charset-clean secret), no callable adapter registered, and
    // no descriptor (id / fallback text / schema name) carries the secret.
    let builder = Agenkit::builder().provider(MockProvider::new("local"));
    let agenkit = register_mcp_tools(builder, runtime.clone())
        .unwrap()
        .build()
        .unwrap();
    assert!(
        !agenkit.tools().contains(&format!("mcp.fixture.{secret}")),
        "no callable adapter may be registered under the secret-derived id"
    );
    let descriptors_json = serde_json::to_string(&agenkit.tools().descriptors()).unwrap();
    assert!(
        !descriptors_json.contains(secret),
        "no tool descriptor (id / text / schema name) may carry the secret: {descriptors_json}"
    );

    let listing = McpToolsTool::new(runtime.clone()).run(McpToolsInput {
        server: Some("fixture".to_string()),
        include_schema: true,
    });
    assert!(
        !listing
            .tools
            .iter()
            .any(|t| t.id.contains(secret) || t.name.contains(secret)),
        "no mcp.tools entry may carry the secret in its id or name"
    );
    let listing_json = serde_json::to_string(&listing).unwrap();
    assert!(
        !listing_json.contains(secret),
        "the secret must never reach the model through the tool name: {listing_json}"
    );

    runtime.shutdown_all().await;
}

// --- Codex round-3 fixes — Group G ----------------------------------------

/// RH2b — a `tools/list_changed` arriving on ONE principal's connection re-denies
/// ANOTHER principal's next **adapter** dispatch, even though that principal's own
/// connection never received the notification. The dirty flag is now SERVER-WIDE
/// (approvals/pins are keyed `(server, tool)`), shared by every per-principal
/// connection, so a notification on principal A's transport dirties the server for
/// principal B; B's next dispatch re-discovers + re-pins B's OWN child (the mutated
/// def) and is re-denied. Pre-fix the flag was per-connection, so B observed a
/// clean flag, skipped rediscovery, and dispatched under the still-approved stale
/// pin.
#[tokio::test]
async fn list_changed_on_one_principal_re_denies_another_via_adapter() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--mutate-relist"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );

    let alice = Principal::anonymous();
    let bob = Principal::from_user(AuthUser::new("bob"));

    // Each principal opens its OWN sandboxed child (pool keyed by (server,
    // principal), C1); each serves the same original `alpha` on its first list.
    let alice_conn = runtime
        .ensure_connected_as("fixture", &alice)
        .await
        .unwrap();
    let _bob_conn = runtime.ensure_connected_as("fixture", &bob).await.unwrap();

    // Approve `alpha` at its original (server-wide) pin.
    let tool = alice_conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    let original_hash = tool.pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);

    // A tools/list_changed arrives on ALICE's transport only (her handler flips the
    // SHARED, server-wide flag). Bob's separate child never receives it directly.
    alice_conn.mark_list_changed();

    // Bob dispatches through an Allow adapter. His own connection's notification was
    // never delivered, but the server-wide dirty state re-discovers + re-pins his
    // child — the `--mutate-relist` re-list serves a changed `alpha`, clearing the
    // approval — so Bob is re-denied (pre-fix Bob ran under the stale pin).
    let adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();

    let err = adapter
        .invoke(serde_json::json!({ "text": "hi" }), &bob)
        .await
        .expect_err("a notification on another principal's connection must re-deny bob");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "the cross-principal re-pin must clear the prior approval"
    );

    runtime.shutdown_all().await;
}

/// RH2b — the same cross-principal re-deny holds through the `mcp.call` escape
/// hatch: a `tools/list_changed` on principal A's connection re-denies principal
/// B's next `mcp.call`, without B's connection receiving the notification directly.
#[tokio::test]
async fn list_changed_on_one_principal_re_denies_another_via_mcp_call() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--mutate-relist"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );

    let alice = Principal::anonymous();
    let bob = Principal::from_user(AuthUser::new("bob"));
    let alice_conn = runtime
        .ensure_connected_as("fixture", &alice)
        .await
        .unwrap();
    let _bob_conn = runtime.ensure_connected_as("fixture", &bob).await.unwrap();

    let original_hash = alice_conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);

    // The notification lands on Alice's connection (the shared server-wide flag).
    alice_conn.mark_list_changed();

    let err = McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "hi" }).as_object().cloned(),
            },
            &bob,
        )
        .await
        .expect_err("a notification on another principal's connection must re-deny bob's mcp.call");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "the cross-principal re-pin must clear the prior approval"
    );

    runtime.shutdown_all().await;
}

/// RH2c — a `tools/list_changed` arriving DURING rediscovery must never let the
/// current dispatch be admitted under the prior approval, and the convergence loop
/// must not livelock. The `--relist-notify` fixture serves a DISTINCT `alpha` on
/// every re-list AND re-arms `tools/list_changed` after each one (on the wire
/// before the page-2 response that completes the client's `list_all_tools`, so the
/// dirty flag is reliably re-set before each rediscovery resolves). The dispatch's
/// bounded convergence loop therefore keeps re-discovering until it hits the retry
/// limit and **fails closed** — never returning a connection to admission while the
/// server is still dirty, and terminating (no livelock).
#[tokio::test]
async fn list_changed_during_rediscovery_never_admits_and_converges() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--relist-notify"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    let conn = runtime.connection("fixture").unwrap();
    let original_hash = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);

    // The server announced a tools/list_changed; each rediscovery the dispatch runs
    // re-arms it (the fixture mutates + re-notifies on every re-list), so the
    // bounded loop never finds the server clean and fails closed rather than
    // admitting under the stale pin.
    conn.mark_list_changed();

    let err = McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "hi" }).as_object().cloned(),
            },
            &Principal::anonymous(),
        )
        .await
        .expect_err(
            "a dispatch must never be admitted while the server keeps re-arming list_changed",
        );
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        err.to_string().contains("unconverged"),
        "the bounded convergence loop must fail closed, not livelock: {err}"
    );
    // The prior approval was cleared by a changed re-list — never silently kept.
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "a changed re-list must clear the prior approval"
    );

    runtime.shutdown_all().await;
}

// --- Codex round-4 fix — Group H ------------------------------------------

/// Arm one principal's child to mutate `alpha` on its next re-list (a
/// per-connection rug-pull trigger). A direct `call_tool` bypasses admission /
/// rediscovery so it does not disturb the pin store or advance the generation.
async fn arm_relist(conn: &McpConnection) {
    conn.call_tool(
        "alpha",
        serde_json::json!({ "__arm_relist__": true })
            .as_object()
            .cloned(),
    )
    .await
    .expect("arming call_tool succeeds");
}

/// RH2b-gen — the server-wide `tools/list_changed` signal must be observed
/// INDEPENDENTLY by every principal's connection; one dispatcher cannot consume
/// it for another. The pre-fix dirty flag was a single CONSUMABLE `AtomicBool`:
/// `take_list_changed()` swapped it false, but a rediscovery only re-listed the
/// *current* principal's child. So if a notification fires (server-wide) and Bob
/// dispatches FIRST, Bob clears the flag by rediscovering his (unchanged) child;
/// the notified principal Alice then sees a CLEAN flag, skips rediscovery, and
/// dispatches under the STALE approval — even though HER child's def changed.
///
/// This test arms ONLY Alice's child to mutate on re-list; Bob's child re-lists
/// UNCHANGED. After the (server-wide) notification, Bob dispatches first
/// (admitted, consuming nothing now), then Alice's next **adapter** dispatch must
/// re-discover HER own child, observe the changed pin, clear the approval, and be
/// re-denied. (Pre-fix Alice was admitted — the regression.)
#[tokio::test]
async fn list_changed_consumed_by_other_principal_still_re_denies_notified_via_adapter() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--relist-on-command"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );

    let alice = Principal::anonymous();
    let bob = Principal::from_user(AuthUser::new("bob"));

    // Each principal opens its OWN sandboxed child (pool keyed by (server,
    // principal), C1); both serve the original `alpha` on their first list.
    let alice_conn = runtime
        .ensure_connected_as("fixture", &alice)
        .await
        .unwrap();
    let bob_conn = runtime.ensure_connected_as("fixture", &bob).await.unwrap();

    let tool = alice_conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .clone();
    let original_hash = tool.pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);

    // Arm ONLY Alice's child to mutate `alpha` on its next re-list. Bob's child
    // stays unarmed → its re-list is UNCHANGED.
    arm_relist(&alice_conn).await;

    // A single (server-wide) tools/list_changed fires (the handler does exactly
    // this on the notification). With a consumable bit, the first dispatcher would
    // clear it for everyone.
    alice_conn.mark_list_changed();

    // BOB dispatches FIRST. His child is unchanged, so his rediscovery keeps the
    // approval and he is admitted (correct). Pre-fix this consumed the shared bit.
    let bob_adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();
    bob_adapter
        .invoke(serde_json::json!({ "text": "bob" }), &bob)
        .await
        .expect("bob's unchanged tool still dispatches");
    assert!(
        runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "bob's unchanged rediscovery must not clear the approval"
    );

    // ALICE dispatches NEXT. The notification was on her transport AND her child's
    // def changed. Pre-fix the flag is already clear (Bob consumed it) → Alice
    // skips rediscovery → dispatches under the STALE approval (the bug). Post-fix
    // the generation counter is observed independently per connection, so Alice
    // re-discovers HER (mutated) child, the pin changes, the approval clears, and
    // she is re-denied.
    let alice_adapter = McpToolAdapter::new(
        "mcp.fixture.alpha",
        "fixture",
        &tool,
        runtime.clone(),
        ToolMode::Allow,
        runtime.pins(),
        Arc::new(McpObserver::default()),
    )
    .unwrap();
    let err = alice_adapter
        .invoke(serde_json::json!({ "text": "alice" }), &alice)
        .await
        .expect_err("alice must be re-denied under her own changed pin");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "alice's independent rediscovery must clear the stale approval"
    );

    // Bob's child never received the cross-principal mutation.
    let _ = &bob_conn;
    runtime.shutdown_all().await;
}

/// RH2b-gen — the same cross-principal independence holds through the `mcp.call`
/// escape hatch: a notified principal whose child changed is re-denied even after
/// another principal's (unchanged) dispatch has already "consumed" the
/// server-wide signal.
#[tokio::test]
async fn list_changed_consumed_by_other_principal_still_re_denies_notified_via_mcp_call() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--relist-on-command"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );

    let alice = Principal::anonymous();
    let bob = Principal::from_user(AuthUser::new("bob"));
    let alice_conn = runtime
        .ensure_connected_as("fixture", &alice)
        .await
        .unwrap();
    let _bob_conn = runtime.ensure_connected_as("fixture", &bob).await.unwrap();

    let original_hash = alice_conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);

    // Arm only Alice's child; fire the server-wide notification.
    arm_relist(&alice_conn).await;
    alice_conn.mark_list_changed();

    // Bob's (unchanged) mcp.call dispatches first — admitted, consuming nothing.
    McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "bob" }).as_object().cloned(),
            },
            &bob,
        )
        .await
        .expect("bob's unchanged mcp.call still dispatches");
    assert!(
        runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "bob's unchanged rediscovery must not clear the approval"
    );

    // Alice's next mcp.call re-discovers her own (mutated) child and is re-denied.
    let err = McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "alice" }).as_object().cloned(),
            },
            &alice,
        )
        .await
        .expect_err("alice's mcp.call must be re-denied under her own changed pin");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "alice's independent rediscovery must clear the stale approval"
    );

    runtime.shutdown_all().await;
}

// --- Codex round-5 fix — Group I ------------------------------------------

/// Group I (LOW gen-overflow) — the server-wide `tools/list_changed` generation
/// is an `AtomicU64`. A naive `fetch_add(1)` wraps to `0` at `u64::MAX`, and the
/// convergence check `observed >= target` would then treat an already-observed
/// `u64::MAX` as "caught up" to the wrapped `0` — SKIPPING rediscovery and
/// admitting under a stale pin (a post-overflow stale-pin admit). The fix
/// **saturates** the bump at `u64::MAX` and treats `u64::MAX` as a permanent
/// "always-dirty" poison sentinel that is never caught up, so a post-overflow
/// notification still forces rediscovery and ultimately fails closed.
///
/// This drives the counter to the overflow boundary (a test-only seam, since
/// firing 2^64 notifications is infeasible), arms a rug-pull, fires ONE
/// notification, and asserts the next dispatch re-discovers the mutated def
/// (clearing the stale approval) and is re-denied — rather than skipping the
/// rediscovery and dispatching under the stale pin. PRE-FIX this dispatch is
/// admitted (the regression): `mark_list_changed` wraps `u64::MAX`→`0` and
/// `observed (u64::MAX) >= target (0)` short-circuits the convergence loop.
#[tokio::test]
async fn list_changed_generation_overflow_still_re_denies() {
    let (_dir, root) = workspace();
    let runtime = Arc::new(
        McpRuntime::new(vec![scoped_fixture_config(
            "fixture",
            McpScope::Session,
            &["--relist-on-command"],
        )])
        .unwrap()
        .with_root(root.to_path_buf())
        .with_default_path(host_path()),
    );
    runtime.connect_all().await.unwrap();

    let conn = runtime.connection("fixture").unwrap();
    let original_hash = conn
        .tools()
        .iter()
        .find(|t| t.name == "alpha")
        .unwrap()
        .pin_hash();
    runtime.pins().approve("fixture", "alpha", &original_hash);

    // Arm the rug-pull: this child mutates `alpha` (a Changed pin) on its next
    // re-list, so a rediscovery that actually happens must clear the approval.
    arm_relist(&conn).await;

    // Drive the server-wide generation to the `u64::MAX` overflow boundary (the
    // connection has "caught up" to it), then fire ONE notification. A naive
    // `fetch_add(1)` wraps the server generation to 0 here; the fix saturates it
    // and treats `u64::MAX` as the always-dirty poison sentinel.
    conn.seed_generation_for_test(u64::MAX);
    conn.mark_list_changed();

    // The next dispatch must NOT observe-its-way-clean past the wrap: it
    // re-discovers the (mutated) def, the changed pin clears the stale approval,
    // and the bounded convergence loop fails closed. PRE-FIX the loop short-
    // circuits (`u64::MAX >= 0`), skips rediscovery, and admits under the stale
    // pin — so this call returns `Ok` and the test fails.
    let err = McpCallTool::new(runtime.clone())
        .run(
            McpCallInput {
                server: "fixture".to_string(),
                tool: "alpha".to_string(),
                arguments: serde_json::json!({ "text": "hi" }).as_object().cloned(),
            },
            &Principal::anonymous(),
        )
        .await
        .expect_err("a post-overflow tools/list_changed must never admit under a stale pin");
    assert_eq!(err.kind(), "tool_policy");
    assert!(
        !runtime
            .pins()
            .is_approved("fixture", "alpha", &original_hash),
        "the overflow-boundary rediscovery must clear the stale approval, not skip it"
    );

    runtime.shutdown_all().await;
}
