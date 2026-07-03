//! The MCP host runtime.
//!
//! `McpRuntime` owns the configured server set, the project root + `PATH` used
//! to spawn sandboxed stdio servers, and (in later phases) the live connections,
//! the negotiated capabilities, the description-pin store (D8, in-memory +
//! session-scoped for v1), and the `context_token` handshake that lets the
//! agent-facing `mcp.*` verbs recover caller scope. Phase 0 established the type
//! and config ownership; Phase 1 adds the spawn context + a [`connect`] helper.
//!
//! [`connect`]: McpRuntime::connect

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use http::HeaderName;
use pocopine_agenkit::server::{Principal, SecretString};
use pocopine_agenkit_core::{AgenkitError, AgenkitResult};
use url::Url;

use crate::policy::{ToolApprover, ToolMode};
use crate::tools::network::{HickoryResolver, Resolve};
use crate::tools::secrets::{SecretRuntime, resolve_secret_headers, validate_secret_env_name};

use super::admission::resolve_mode;
use super::client::{McpConnection, McpToolDef};
use super::common::{McpToolEntry, namespaced_tool_id, schema_digest};
use super::config::{McpServerConfig, McpTransportConfig};
use super::loader::load_mcp_config;
use super::observer::{McpAuditEntry, McpObserver};
use super::pins::{PinStatus, PinStore};
use crate::events::FrameworkEvent;

/// Upper bound on consecutive `tools/list_changed` rediscoveries the dispatch
/// path will run before a single dispatch (RH2c). A server that mutates its tool
/// set on *every* re-list (bumping the generation during each rediscovery) would
/// otherwise spin the convergence loop forever; on exceeding the bound the
/// dispatch **fails closed** (the connection's observed generation is left behind
/// the server generation, so the next dispatch retries) rather than being
/// admitted under an unconverged tool set.
const MAX_REDISCOVER_ATTEMPTS: u32 = 8;

/// Poison sentinel for the server-wide `tools/list_changed` generation counter.
/// The counter **saturates** at this value instead of wrapping (see
/// [`McpClientHandler::note_tools_list_changed`](super::handler::McpClientHandler)
/// and [`McpConnection::mark_list_changed`](super::client::McpConnection)), and a
/// `target` generation equal to it is treated as **permanently dirty** — never
/// "caught up" — in the convergence loop below. So a (practically unreachable,
/// 2^64-notification) overflow can never let a dispatch observe-its-way-clean past
/// a wrap-to-`0` and admit under a stale pin: post-overflow the loop keeps
/// re-discovering and ultimately fails closed at the attempt bound (Codex round-5,
/// Group I).
const POISON_GENERATION: u64 = u64::MAX;

/// Pool/lock key: a connection (and its sandboxed child / HTTP client) is
/// **never** shared across principals — a secret-bearing child resolved for one
/// caller identity must not be lent to another (C1). The key pairs the server
/// name with a stable per-principal identity.
type PoolKey = (String, String);

/// Stable per-principal pool key (mirrors the secrets layer's `principal_key`):
/// the authenticated user id, or `"anonymous"`.
fn principal_key(principal: &Principal) -> String {
    principal
        .user()
        .map_or_else(|| "anonymous".to_string(), |user| user.id.clone())
}

/// Fallback `PATH` for a sandboxed stdio server when the host `PATH` is unset.
fn default_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string())
}

/// Host-owned MCP runtime. Cheap to clone (`Arc` internals) so the registration
/// path and per-call tools can share one instance.
#[derive(Clone)]
pub struct McpRuntime {
    inner: Arc<McpRuntimeInner>,
    /// Live, mutable runtime state (the connection pool + published tool index)
    /// shared across clones.
    state: Arc<McpRuntimeState>,
}

#[derive(Clone)]
struct McpRuntimeInner {
    /// Configured servers, keyed by their unique `name`.
    servers: Vec<McpServerConfig>,
    /// Project root used as the sandbox confinement root for stdio servers.
    root: PathBuf,
    /// `PATH` handed to sandboxed stdio children.
    default_path: String,
    /// Resolves per-server secret-handle env into values at spawn (D6). `None`
    /// means a server config with a non-empty env map cannot connect (fail
    /// closed) — secret handles require a configured runtime.
    secret_runtime: Option<Arc<SecretRuntime>>,
    /// DNS resolver used to resolve→pin remote (HTTP) endpoints (T6). `None`
    /// builds the production hickory resolver lazily on the first HTTP connect;
    /// tests inject a mock via [`McpRuntime::with_http_resolver`].
    http_resolver: Option<Arc<dyn Resolve>>,
    /// Host approver for `Ask`-admitted tools without an approved current pin
    /// (M1d). `None` = fail closed, exactly the pre-approver semantics.
    approver: Option<Arc<dyn ToolApprover>>,
}

impl std::fmt::Debug for McpRuntimeInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpRuntimeInner")
            .field("servers", &self.servers.len())
            .field("root", &self.root)
            .field("secret_runtime", &self.secret_runtime.is_some())
            .finish_non_exhaustive()
    }
}

/// Interior-mutable runtime state. The connection pool is in-memory and
/// session-scoped for v1 (a scope-aware, per-server-locked pool lands in
/// Phase 6); the tool index is published once by
/// [`register_mcp_tools`](super::registry::register_mcp_tools).
#[derive(Default)]
struct McpRuntimeState {
    /// Pooled live connections keyed by `(server, principal)` (C1): a
    /// secret-bearing connection is never shared across caller identities.
    connections: Mutex<HashMap<PoolKey, Arc<McpConnection>>>,
    /// Per-`(server, principal)` async connect lock (Phase 6): serializes
    /// concurrent first-connects to the same pool key so the pool never opens
    /// two transports for one key. The entry is created on demand.
    connect_locks: Mutex<HashMap<PoolKey, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-**server** async **rediscovery** lock (RH2/RH2b): serializes the
    /// `tools/list_changed` rediscover → re-pin → revoke-missing **across all
    /// principals** of a server, because the pin store + the dirty flag are
    /// server-wide (keyed by server / `(server, tool)`), not per-principal. A
    /// concurrent dispatch — by the *same or a different* principal — blocks on
    /// this gate rather than observing a cleared dirty flag while the re-pin is
    /// still in flight and dispatching under a stale approval. Distinct from
    /// `connect_locks` and only ever held across `rediscover_into` /
    /// `repin_snapshot` (never across a `connect`), and acquired only **after**
    /// `ensure_connected_as` has released the connect lock, so the two locks are
    /// never held together and can't deadlock. The entry is created on demand.
    rediscover_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Per-**server** `tools/list_changed` generation counter (RH2b-gen).
    /// Approvals/pins are server-wide (keyed `(server, tool)`), so a notification
    /// arriving on **any** principal's connection must be observed by **every**
    /// principal — not only the connection that received it. The runtime mints one
    /// `Arc<AtomicU64>` per server name (created on demand) and hands the **same**
    /// counter to every per-principal connection at connect; a notification
    /// **increments** it (never clears it). Because it is a monotonic generation
    /// rather than a consumable bit, each connection tracks its own observed
    /// generation and catches up independently — one principal's dispatch cannot
    /// consume the notification for another. Entries persist for the runtime's life
    /// (a `fork` gets a fresh, empty map).
    server_generations: Mutex<HashMap<String, Arc<AtomicU64>>>,
    /// The published, secret-free tool index read by the `mcp.tools` verb.
    tool_index: Mutex<Vec<McpToolEntry>>,
    /// The session-scoped tool-definition pin store (D8/T2): TOFU + approvals,
    /// shared with every adapter built off this runtime.
    pins: Arc<PinStore>,
    /// The session-scoped eventing + audit collector (Phase 7): a
    /// [`FrameworkEvent`] per connect/call plus an audit row per
    /// call/approval/hash-change/connect. Shared with every adapter built off
    /// this runtime; a `fork` starts with a fresh (empty) observer.
    observer: Arc<McpObserver>,
}

impl std::fmt::Debug for McpRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `McpConnection` is not `Debug` (it owns an rmcp `RunningService`), so
        // summarize rather than derive: server count + pool size.
        f.debug_struct("McpRuntime")
            .field("servers", &self.inner.servers.len())
            .field("root", &self.inner.root)
            .field(
                "connections",
                &self.state.connections.lock().map(|c| c.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl McpRuntime {
    /// Build a runtime from a set of configured servers. Rejects duplicate
    /// server names (a server name is unique within a registry). The sandbox
    /// root defaults to the current directory; set it with [`with_root`].
    ///
    /// [`with_root`]: McpRuntime::with_root
    pub fn new(servers: Vec<McpServerConfig>) -> AgenkitResult<Self> {
        for (index, server) in servers.iter().enumerate() {
            if server.name.trim().is_empty() {
                return Err(AgenkitError::config("mcp server name must not be empty"));
            }
            if servers[..index]
                .iter()
                .any(|other| other.name == server.name)
            {
                return Err(AgenkitError::config(format!(
                    "duplicate mcp server name `{}`",
                    server.name
                )));
            }
        }
        Ok(Self {
            inner: Arc::new(McpRuntimeInner {
                servers,
                root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                default_path: default_path(),
                secret_runtime: None,
                http_resolver: None,
                approver: None,
            }),
            state: Arc::new(McpRuntimeState::default()),
        })
    }

    /// An empty runtime with no configured servers.
    pub fn empty() -> Self {
        Self::new(Vec::new()).expect("empty server set is always valid")
    }

    /// Set the sandbox confinement root used to spawn stdio servers.
    pub fn with_root(mut self, root: impl Into<PathBuf>) -> Self {
        Arc::make_mut(&mut self.inner).root = root.into();
        self
    }

    /// Override the `PATH` handed to sandboxed stdio children.
    pub fn with_default_path(mut self, path: impl Into<String>) -> Self {
        Arc::make_mut(&mut self.inner).default_path = path.into();
        self
    }

    /// Attach the [`SecretRuntime`] that resolves per-server secret-handle env
    /// into values at spawn (D6). Without it, a server whose stdio `env` map is
    /// non-empty fails to connect (fail closed).
    pub fn with_secret_runtime(mut self, runtime: Arc<SecretRuntime>) -> Self {
        Arc::make_mut(&mut self.inner).secret_runtime = Some(runtime);
        self
    }

    /// Inject the DNS resolver used to resolve→pin remote (HTTP) endpoints
    /// (tests use a mock; production lazily builds the hickory resolver).
    pub fn with_http_resolver(mut self, resolver: Arc<dyn Resolve>) -> Self {
        Arc::make_mut(&mut self.inner).http_resolver = Some(resolver);
        self
    }

    /// Install the host approver consulted when an `Ask`-admitted tool has no
    /// approved current pin (M1d). Approval records the pin for the session;
    /// without an approver such calls fail closed.
    pub fn with_approver(mut self, approver: Arc<dyn ToolApprover>) -> Self {
        Arc::make_mut(&mut self.inner).approver = Some(approver);
        self
    }

    /// The installed host approver, if any.
    pub(crate) fn approver(&self) -> Option<Arc<dyn ToolApprover>> {
        self.inner.approver.clone()
    }

    /// The DNS resolver for remote endpoints, building the production hickory
    /// resolver on first use when none was injected.
    fn http_resolver(&self) -> AgenkitResult<Arc<dyn Resolve>> {
        if let Some(resolver) = &self.inner.http_resolver {
            return Ok(resolver.clone());
        }
        let resolver = HickoryResolver::new().map_err(|err| {
            AgenkitError::config(format!("mcp http DNS resolver init failed: {err}"))
        })?;
        Ok(Arc::new(resolver))
    }

    /// Build a runtime from a `.agenkitty/mcp.json` (`mcpServers` shape) under
    /// `dir`. A missing file yields an empty runtime (MCP is opt-in); a present
    /// but malformed file is a `config` error. The sandbox root is set to `dir`.
    pub fn from_config_dir(dir: impl Into<PathBuf>) -> AgenkitResult<Self> {
        let dir = dir.into();
        let servers = load_mcp_config(&dir)?;
        Ok(Self::new(servers)?.with_root(dir))
    }

    /// Build a runtime from an in-memory `mcpServers` JSON document (the builder
    /// API counterpart to [`from_config_dir`](Self::from_config_dir)).
    pub fn from_mcp_servers_json(json: &str) -> AgenkitResult<Self> {
        let servers = super::loader::McpServersFile::from_json_str(json)?.into_configs()?;
        Self::new(servers)
    }

    /// The session-scoped tool-definition pin store (D8/T2), shared with every
    /// adapter built off this runtime.
    pub fn pins(&self) -> Arc<PinStore> {
        self.state.pins.clone()
    }

    /// The session-scoped eventing + audit collector (Phase 7), shared with every
    /// adapter built off this runtime.
    pub fn observer(&self) -> Arc<McpObserver> {
        self.state.observer.clone()
    }

    /// A snapshot of the framework events recorded this session (connect + call).
    pub fn events(&self) -> Vec<FrameworkEvent> {
        self.state.observer.events()
    }

    /// A snapshot of the audit entries recorded this session
    /// (connect / call / approval / hash-change).
    pub fn audit(&self) -> Vec<McpAuditEntry> {
        self.state.observer.audit()
    }

    /// The configured servers.
    pub fn servers(&self) -> &[McpServerConfig] {
        &self.inner.servers
    }

    /// Look up a configured server by name.
    pub fn server(&self, name: &str) -> Option<&McpServerConfig> {
        self.inner.servers.iter().find(|server| server.name == name)
    }

    /// The sandbox confinement root.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Open a connection to a configured server by name: spawn (stdio) →
    /// handshake → protocol-validate → discover. The caller owns the returned
    /// [`McpConnection`] and is responsible for closing it.
    ///
    /// A live connection pool (per-server lock, scope-aware teardown) lands in
    /// Phase 6; Phase 1 connects on demand and hands the connection back.
    pub async fn connect(&self, name: &str) -> AgenkitResult<McpConnection> {
        self.connect_as(name, &Principal::anonymous()).await
    }

    /// Open a connection as `principal`, resolving the server's secret-handle env
    /// into the child for that caller identity (D6). The connection is **not**
    /// pooled — the caller owns it.
    pub async fn connect_as(
        &self,
        name: &str,
        principal: &Principal,
    ) -> AgenkitResult<McpConnection> {
        let config = self
            .server(name)
            .ok_or_else(|| AgenkitError::not_found(format!("unknown mcp server `{name}`")))?
            .clone();
        let observer = self.observer();
        observer.connect_started(name);
        let transport_kind = config.transport.kind();
        // The whole connect (secret resolution + transport build + handshake)
        // is one attempt: any failure terminates the `connect_started` with a
        // `connect_failed` event/audit row.
        let result = self.connect_transport(&config, principal).await;
        match &result {
            Ok(connection) => observer.connect_completed(
                name,
                transport_kind,
                connection.protocol_version(),
                connection.tools().len(),
            ),
            Err(err) => observer.connect_failed(name, err),
        }
        result
    }

    /// Resolve per-server secrets and open the transport (no eventing — the
    /// caller [`connect_as`](Self::connect_as) brackets this with connect events).
    async fn connect_transport(
        &self,
        config: &McpServerConfig,
        principal: &Principal,
    ) -> AgenkitResult<McpConnection> {
        // The server-wide generation counter is shared with every per-principal
        // connection to this server (RH2b-gen): a `tools/list_changed` on one
        // connection bumps the generation for all principals, and each catches up
        // independently.
        let server_generation = self.server_generation(&config.name);
        match &config.transport {
            McpTransportConfig::Stdio { .. } => {
                let secrets = self.resolve_secret_env(config, principal).await?;
                McpConnection::connect_with_secrets_shared(
                    config,
                    &self.inner.root,
                    &self.inner.default_path,
                    secrets,
                    server_generation,
                )
                .await
            }
            McpTransportConfig::Http { .. } => {
                let headers = self.resolve_secret_headers(config, principal).await?;
                let resolver = self.http_resolver()?;
                McpConnection::connect_http_shared(config, headers, resolver, server_generation)
                    .await
            }
        }
    }

    /// Resolve a remote server's `headers` map (header name → secret-handle id)
    /// into the credential headers injected at the transport edge (D6/D10). The
    /// handles resolve through the [`SecretRuntime`] scoped to
    /// `target_tool = mcp.<server>` and `destination = <endpoint host>`, so a
    /// handle minted for one server cannot be replayed into another. An empty
    /// header map needs no runtime; a non-empty one without a configured runtime
    /// fails closed.
    async fn resolve_secret_headers(
        &self,
        config: &McpServerConfig,
        principal: &Principal,
    ) -> AgenkitResult<Vec<(HeaderName, SecretString)>> {
        let McpTransportConfig::Http { url, headers } = &config.transport else {
            return Ok(Vec::new());
        };
        if headers.is_empty() {
            return Ok(Vec::new());
        }
        let runtime = self.inner.secret_runtime.as_ref().ok_or_else(|| {
            AgenkitError::tool_policy(format!(
                "mcp server `{}` declares secret headers but no SecretRuntime is configured",
                config.name
            ))
        })?;
        let target_tool = format!("mcp.{}", config.name);
        let host = Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string));
        let mut resolved = Vec::new();
        for (header_name, handle_id) in resolve_secret_headers(headers)? {
            let secret = runtime
                .resolve_handle(&handle_id, principal, &target_tool, host.as_deref())
                .await?;
            resolved.push((header_name, secret));
        }
        Ok(resolved)
    }

    /// Resolve a server's stdio `env` map (env-var name → secret-handle id) into
    /// the per-server env values to inject at spawn (D6). The handles resolve
    /// through the [`SecretRuntime`] scoped to `target_tool = mcp.<server>` and
    /// `destination = <env var>`, so a handle minted for one server/var cannot be
    /// replayed into another. An empty env map needs no runtime; a non-empty one
    /// without a configured runtime fails closed.
    async fn resolve_secret_env(
        &self,
        config: &McpServerConfig,
        principal: &Principal,
    ) -> AgenkitResult<BTreeMap<String, SecretString>> {
        let McpTransportConfig::Stdio { env, .. } = &config.transport else {
            return Ok(BTreeMap::new());
        };
        if env.is_empty() {
            return Ok(BTreeMap::new());
        }
        let runtime = self.inner.secret_runtime.as_ref().ok_or_else(|| {
            AgenkitError::tool_policy(format!(
                "mcp server `{}` declares secret env but no SecretRuntime is configured",
                config.name
            ))
        })?;
        let target_tool = format!("mcp.{}", config.name);
        let mut resolved = BTreeMap::new();
        for (env_name, handle_id) in env {
            validate_secret_env_name(env_name)?;
            let secret = runtime
                .resolve_handle(handle_id, principal, &target_tool, Some(env_name.as_str()))
                .await?;
            resolved.insert(env_name.clone(), secret);
        }
        Ok(resolved)
    }

    /// Connect to a configured server (if not already pooled), store the live
    /// connection in the runtime pool, and return a shared handle.
    pub async fn ensure_connected(&self, name: &str) -> AgenkitResult<Arc<McpConnection>> {
        self.ensure_connected_as(name, &Principal::anonymous())
            .await
    }

    /// Connect-if-absent as `principal` (so the per-server secret env is resolved
    /// for that caller identity, D6), pool the connection **keyed by
    /// `(server, principal)`** (C1 — never shared across identities), and return a
    /// shared handle.
    ///
    /// A **per-`(server, principal)` async lock** (Phase 6) serializes concurrent
    /// first-connects to the same pool key: the loser of a race waits on the lock,
    /// then re-checks the pool and reuses the winner's connection rather than
    /// opening (and then dropping) a second transport.
    pub async fn ensure_connected_as(
        &self,
        name: &str,
        principal: &Principal,
    ) -> AgenkitResult<Arc<McpConnection>> {
        if let Some(existing) = self.connection_as(name, principal) {
            return Ok(existing);
        }
        // Serialize the connect for this pool key so two callers never open two
        // transports. The lock is acquired *across* the await (unlike the pool
        // mutex, which is only ever held for map lookups).
        let lock = self.connect_lock(name, principal);
        let _guard = lock.lock().await;
        // Re-check under the lock: a racing caller may have just connected.
        if let Some(existing) = self.connection_as(name, principal) {
            return Ok(existing);
        }
        let connection = Arc::new(self.connect_as(name, principal).await?);
        self.state
            .connections
            .lock()
            .expect("mcp connection pool mutex poisoned")
            .insert(
                (name.to_string(), principal_key(principal)),
                connection.clone(),
            );
        Ok(connection)
    }

    /// Ensure a connection for `principal`, then — if the server announced a
    /// `tools/list_changed` since the last check — **re-discover + re-pin-check**
    /// it before returning, so a changed/added/removed tool can never be
    /// dispatched under a stale approval (T2/H1). This is the entry the dispatch
    /// paths (the imported-tool adapter and `mcp.call`) use.
    pub(crate) async fn ready_connection_as(
        &self,
        name: &str,
        principal: &Principal,
    ) -> AgenkitResult<Arc<McpConnection>> {
        let connection = self.ensure_connected_as(name, principal).await?;
        // Serialize the rediscovery **per server** (across all principals) and hold
        // the gate across the whole repin/check-and-rediscover (RH2/RH2b-gen): the
        // pin store + generation counter are server-wide, so a concurrent dispatcher
        // — by the same OR a different principal — blocks here rather than observing
        // a half-updated pin store mid-rediscovery and dispatching under a stale
        // approval. By the time it acquires the lock the re-pin is complete and its
        // own observed generation will be re-checked, so every dispatcher awaits the
        // gate before its `check_admission`/`admit` decision. Acquired *after*
        // `ensure_connected_as` released the connect lock, so the two locks are never
        // held together (no deadlock).
        let lock = self.rediscover_lock(name);
        let _guard = lock.lock().await;

        // RH2b — a freshly-opened per-principal connection observes its OWN
        // discovered tool set against the shared (server-wide) pin store before it
        // can dispatch: the approval is keyed `(server, tool)`, so if THIS
        // principal's child serves a definition different from the approved one the
        // observation clears the approval (fail-closed) — without this a new
        // connection would dispatch against the old registered pin. Done exactly
        // once per connection (atomic guard), against the connect-time snapshot
        // (no extra round-trip).
        if connection.mark_repinned() {
            self.repin_snapshot(name, &connection);
        }

        // RH2b-gen / RH2c — converge this connection's observed generation up to the
        // server-wide generation, looping until it has caught up AFTER a completed
        // rediscovery. The generation is a monotonic counter bumped by a
        // `tools/list_changed` notification on **any** principal's connection and
        // **never consumed**, so it is observed INDEPENDENTLY by every principal's
        // connection: one dispatcher catching up its own connection cannot clear the
        // notification for another (RH2b-gen). The target is read BEFORE each
        // rediscovery; a notification arriving *during* a rediscovery bumps the
        // generation past `target`, so the loop re-checks and rediscovers again
        // (never admitted under the previous approval, RH2c). Bounded by
        // `MAX_REDISCOVER_ATTEMPTS` to avoid a livelock against a server that bumps
        // the generation on every re-list; on exceeding the bound the dispatch
        // **fails closed** (observed generation left behind, so the next dispatch
        // retries). A rediscovery error propagates *before* `set_observed_generation`,
        // so a failed re-pin likewise leaves the connection behind (fail-closed).
        let mut attempts = 0u32;
        loop {
            let target = connection.list_changed_generation();
            // `POISON_GENERATION` (the counter saturated at `u64::MAX`) is
            // **never** "caught up" — it is treated as permanently dirty, so an
            // overflowed generation forces continued rediscovery and ultimately
            // fails closed at the attempt bound below rather than admitting under
            // a stale pin (Group I). A non-poison `target` uses the normal
            // monotonic catch-up check.
            if target != POISON_GENERATION && connection.observed_generation() >= target {
                break;
            }
            attempts += 1;
            if attempts > MAX_REDISCOVER_ATTEMPTS {
                return Err(AgenkitError::tool_policy(format!(
                    "mcp server `{name}` repeatedly signalled tools/list_changed; refusing to \
                     dispatch under an unconverged tool set (re-approval required)"
                )));
            }
            self.rediscover_into(name, &connection, principal).await?;
            // Mark this connection as having observed `target` only after the
            // rediscovery succeeded. A notification that arrived during the
            // rediscovery bumped the generation past `target`, so the next loop
            // iteration re-checks and rediscovers again (never admits while behind).
            connection.set_observed_generation(target);
        }
        Ok(connection)
    }

    /// The per-`(server, principal)` connect lock, created on demand.
    fn connect_lock(&self, name: &str, principal: &Principal) -> Arc<tokio::sync::Mutex<()>> {
        self.state
            .connect_locks
            .lock()
            .expect("mcp connect-lock mutex poisoned")
            .entry((name.to_string(), principal_key(principal)))
            .or_default()
            .clone()
    }

    /// The per-**server** rediscovery lock, created on demand (RH2/RH2b).
    fn rediscover_lock(&self, name: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.state
            .rediscover_locks
            .lock()
            .expect("mcp rediscover-lock mutex poisoned")
            .entry(name.to_string())
            .or_default()
            .clone()
    }

    /// The per-**server** `tools/list_changed` generation counter, created on
    /// demand (RH2b-gen). Every per-principal connection to `name` is handed this
    /// same counter at connect, so a notification on one transport bumps the
    /// generation for all — and each connection catches up independently.
    fn server_generation(&self, name: &str) -> Arc<AtomicU64> {
        self.state
            .server_generations
            .lock()
            .expect("mcp server-generation mutex poisoned")
            .entry(name.to_string())
            .or_default()
            .clone()
    }

    /// Observe a connection's own connect-time tool snapshot against the shared
    /// (server-wide) pin store (RH2b). A definition that differs from the approved
    /// one is a `Changed` observation that **clears** the approval (fail-closed) —
    /// so a per-principal child serving a different def than was approved can't
    /// dispatch against the stale pin. Approvals are **never granted** here (only
    /// the operator-blessed registration approves); a matching def is `Unchanged`
    /// and keeps its approval. Must be called under the per-server rediscovery lock
    /// (it shares the pin store with `rediscover_into`).
    fn repin_snapshot(&self, name: &str, connection: &McpConnection) {
        for tool in connection.tools() {
            let status = self.state.pins.observe(name, &tool.name, &tool.pin_hash());
            if status == PinStatus::Changed {
                self.state.observer.hash_change(name, &tool.name, status);
                tracing::warn!(
                    target: "pocopine.log",
                    server = name,
                    tool = tool.name.as_str(),
                    "mcp connection's tool definition differs from the approved pin; \
                     approval cleared, re-approval required (newly-opened connection)"
                );
            }
        }
    }

    /// Connect to every configured server, populating the pool + discovering
    /// tools. A per-server connection failure is logged and skipped (one bad
    /// server must not block the rest); the first hard error short-circuits only
    /// if you prefer fail-fast — here we are lenient.
    pub async fn connect_all(&self) -> AgenkitResult<()> {
        self.connect_all_as(&Principal::anonymous()).await
    }

    /// Connect to every configured server as `principal` (resolving each
    /// server's secret env for that caller identity, D6). A per-server failure is
    /// logged and skipped (lenient).
    pub async fn connect_all_as(&self, principal: &Principal) -> AgenkitResult<()> {
        let names: Vec<String> = self.inner.servers.iter().map(|s| s.name.clone()).collect();
        for name in names {
            if let Err(err) = self.ensure_connected_as(&name, principal).await {
                tracing::warn!(
                    target: "pocopine.log",
                    server = name.as_str(),
                    error = %err,
                    "mcp server failed to connect; skipping"
                );
            }
        }
        Ok(())
    }

    /// A shared handle to a pooled connection for `name` (any principal). Tool
    /// definitions and protocol metadata are principal-independent, so this is
    /// fine for metadata (`mcp.servers`) and for tests that only connect
    /// anonymously; per-principal dispatch must use
    /// [`connection_as`](Self::connection_as).
    pub fn connection(&self, name: &str) -> Option<Arc<McpConnection>> {
        self.state
            .connections
            .lock()
            .expect("mcp connection pool mutex poisoned")
            .iter()
            .find(|((server, _principal), _conn)| server == name)
            .map(|(_key, conn)| conn.clone())
    }

    /// A shared handle to the pooled connection for `(server, principal)` — the
    /// exact, identity-scoped lookup the connect/dispatch paths use (C1).
    pub fn connection_as(&self, name: &str, principal: &Principal) -> Option<Arc<McpConnection>> {
        self.state
            .connections
            .lock()
            .expect("mcp connection pool mutex poisoned")
            .get(&(name.to_string(), principal_key(principal)))
            .cloned()
    }

    /// Whether any live connection to `name` is currently pooled.
    pub fn is_connected(&self, name: &str) -> bool {
        self.connection(name).is_some()
    }

    /// A snapshot of all pooled connections, sorted by server name (a
    /// deterministic order for the single id-minting pass in
    /// [`register_mcp_tools`](super::registry::register_mcp_tools)).
    pub fn connections_snapshot(&self) -> Vec<Arc<McpConnection>> {
        let mut connections: Vec<Arc<McpConnection>> = self
            .state
            .connections
            .lock()
            .expect("mcp connection pool mutex poisoned")
            .values()
            .cloned()
            .collect();
        connections.sort_by(|a, b| a.server_name().cmp(b.server_name()));
        connections
    }

    /// Publish the secret-free tool index (called once by
    /// [`register_mcp_tools`](super::registry::register_mcp_tools)) so the
    /// `mcp.tools` verb reports exactly the registered ids.
    pub fn publish_tool_index(&self, index: Vec<McpToolEntry>) {
        *self
            .state
            .tool_index
            .lock()
            .expect("mcp tool index mutex poisoned") = index;
    }

    /// The published tool index (empty until [`Self::publish_tool_index`]).
    pub fn tool_index(&self) -> Vec<McpToolEntry> {
        self.state
            .tool_index
            .lock()
            .expect("mcp tool index mutex poisoned")
            .clone()
    }

    // --- Phase 6: lifecycle & scale ---------------------------------------

    /// Re-derive a runtime for a forked / resumed session.
    ///
    /// The returned runtime shares the **immutable configuration** (servers,
    /// root, `PATH`, secret runtime, DNS resolver) but starts with **fresh live
    /// state**: an empty connection pool, an empty tool index, and a new
    /// (unapproved) pin store. No live transport is ever copied — a forked
    /// session re-connects from config on demand (and re-pins/re-approves),
    /// which is the only correct behaviour for a child process / resumed thread
    /// that must not share another session's open file descriptors.
    pub fn fork(&self) -> McpRuntime {
        McpRuntime {
            inner: self.inner.clone(),
            state: Arc::new(McpRuntimeState::default()),
        }
    }

    /// Remove the anonymous-principal pooled connection for `name` (Phase 6),
    /// returning it if one was open. The returned handle is dropped by the caller
    /// (or gracefully closed via [`McpConnection::close`] when it is the sole
    /// owner); dropping the last `Arc` group-SIGKILLs + reaps the stdio child.
    /// To evict a specific identity use [`evict_as`](Self::evict_as).
    pub fn evict(&self, name: &str) -> Option<Arc<McpConnection>> {
        self.evict_as(name, &Principal::anonymous())
    }

    /// Remove the pooled connection for `(server, principal)` (C1-aware), if open.
    pub fn evict_as(&self, name: &str, principal: &Principal) -> Option<Arc<McpConnection>> {
        self.state
            .connections
            .lock()
            .expect("mcp connection pool mutex poisoned")
            .remove(&(name.to_string(), principal_key(principal)))
    }

    /// Remove **every** pooled connection for server `name` (all principals),
    /// returning them. Used by session teardown so an authenticated principal's
    /// session connection isn't leaked past session end.
    pub fn evict_all(&self, name: &str) -> Vec<Arc<McpConnection>> {
        let mut pool = self
            .state
            .connections
            .lock()
            .expect("mcp connection pool mutex poisoned");
        let keys: Vec<PoolKey> = pool
            .keys()
            .filter(|(server, _principal)| server == name)
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|key| pool.remove(&key))
            .collect()
    }

    /// Tear down every **session-scoped** connection (Phase 6): at session end a
    /// `session`-scope server is closed + evicted, while a `project`-scope server
    /// persists in the pool. Each torn-down connection is closed gracefully when
    /// this runtime is its sole owner, else dropped (Drop reaps the child).
    pub async fn teardown_session(&self) {
        let session_names: Vec<String> = self
            .inner
            .servers
            .iter()
            .filter(|s| s.scope.is_session())
            .map(|s| s.name.clone())
            .collect();
        for name in session_names {
            for connection in self.evict_all(&name) {
                close_or_drop(connection).await;
            }
        }
    }

    /// Close + evict **all** pooled connections (e.g. on runtime shutdown). Each
    /// is closed gracefully when this runtime is its sole owner, else dropped.
    pub async fn shutdown_all(&self) {
        let pooled: Vec<Arc<McpConnection>> = {
            let mut pool = self
                .state
                .connections
                .lock()
                .expect("mcp connection pool mutex poisoned");
            pool.drain().map(|(_, conn)| conn).collect()
        };
        for connection in pooled {
            close_or_drop(connection).await;
        }
    }

    /// Reconnect a server after a dropped / dead transport (Phase 6): evict any
    /// pooled connection and connect fresh, returning the new handle. Mirrors the
    /// recovery path when a request fails on a half-dead connection.
    pub async fn reconnect(&self, name: &str) -> AgenkitResult<Arc<McpConnection>> {
        self.reconnect_as(name, &Principal::anonymous()).await
    }

    /// Reconnect as `principal` (re-resolving its secret env/headers, D6).
    pub async fn reconnect_as(
        &self,
        name: &str,
        principal: &Principal,
    ) -> AgenkitResult<Arc<McpConnection>> {
        if let Some(stale) = self.evict_as(name, principal) {
            close_or_drop(stale).await;
        }
        self.ensure_connected_as(name, principal).await
    }

    /// Handle a `tools/list_changed` notification (T2): re-discover the server's
    /// tools and **re-pin-check** them, never silently adopting the swap.
    ///
    /// Each freshly-discovered tool is observed against the pin store: an
    /// unchanged definition stays approved, but a *changed* one (a rug pull /
    /// poisoned swap) clears its prior approval, so an already-registered adapter
    /// pinned to the old hash is disabled pending re-approval. Returns the
    /// per-tool [`PinStatus`] outcomes (for tracing / host re-approval UX).
    pub async fn rediscover(&self, name: &str) -> AgenkitResult<Vec<(String, PinStatus)>> {
        self.rediscover_as(name, &Principal::anonymous()).await
    }

    /// Re-discover + re-pin-check as `principal` (Phase 6, T2). Advances this
    /// connection's observed generation up to the current server generation (we are
    /// re-discovering now), so a subsequent dispatch on the same connection does
    /// not redundantly re-list.
    pub async fn rediscover_as(
        &self,
        name: &str,
        principal: &Principal,
    ) -> AgenkitResult<Vec<(String, PinStatus)>> {
        let connection = self.ensure_connected_as(name, principal).await?;
        // Serialize host-driven rediscovery with the dispatch-driven path on the
        // same per-**server** gate (RH2/RH2b-gen), so a `mcp.*` dispatch by any
        // principal can't race a host `rediscover` and observe a half-updated pin
        // store.
        let lock = self.rediscover_lock(name);
        let _guard = lock.lock().await;
        // Capture the generation BEFORE re-listing, advance the observed generation
        // only on success (so a failed re-pin leaves this connection behind and its
        // next dispatch retries — fail-closed; mirrors `ready_connection_as`). The
        // generation is server-wide and monotonic, so this only advances THIS
        // connection's observed value — other principals' connections still catch up
        // on their own dispatch (never consumed here, RH2b-gen).
        let target = connection.list_changed_generation();
        let outcomes = self.rediscover_into(name, &connection, principal).await?;
        connection.set_observed_generation(target);
        Ok(outcomes)
    }

    /// The shared re-discover → re-pin core (T2/H1), operating on an already-open
    /// `connection`. Each freshly-listed tool is observed against the pin store —
    /// a **changed** definition clears its prior approval (so an adapter / verb
    /// pinned to the old hash is re-denied until re-approval, never silently
    /// adopting the swap). Tools that **disappeared** have their approvals revoked
    /// ([`PinStore::revoke_missing`]), and the published index for the server is
    /// rebuilt so `mcp.tools` reflects the new set. **Never re-approves** here:
    /// only the operator-blessed registration auto-approves an allowlisted tool;
    /// a re-listed (even later-stable) definition must be re-approved by the host,
    /// so a rug pull can't be adopted on a second sighting.
    async fn rediscover_into(
        &self,
        name: &str,
        connection: &McpConnection,
        _principal: &Principal,
    ) -> AgenkitResult<Vec<(String, PinStatus)>> {
        let config = self.server(name).cloned();
        let fresh = connection.rediscover().await?;

        // Revoke approvals for tools that vanished from the listing.
        let present: HashSet<String> = fresh.iter().map(|tool| tool.name.clone()).collect();
        for revoked in self.state.pins.revoke_missing(name, &present) {
            tracing::warn!(
                target: "pocopine.log",
                server = name,
                tool = revoked.as_str(),
                "mcp tool disappeared on tools/list_changed; approval revoked"
            );
        }

        let mut outcomes = Vec::with_capacity(fresh.len());
        for tool in &fresh {
            let status = self.state.pins.observe(name, &tool.name, &tool.pin_hash());
            self.state.observer.hash_change(name, &tool.name, status);
            if status == PinStatus::Changed {
                tracing::warn!(
                    target: "pocopine.log",
                    server = name,
                    tool = tool.name.as_str(),
                    "mcp tool definition changed on tools/list_changed; \
                     approval cleared, re-approval required (not silently adopted)"
                );
            }
            outcomes.push((tool.name.clone(), status));
        }

        // Rebuild the published index for this server so `mcp.tools` reflects the
        // new set (added tools appear, removed tools drop out, changed digests
        // update). Dispatch is gated by the pin store above regardless.
        self.refresh_index_for_server(name, config.as_ref(), &fresh);
        Ok(outcomes)
    }

    /// Rebuild the published tool index entries for `name` from `fresh`,
    /// re-minting ids against the other servers' entries (capability pre-filter:
    /// `Deny` tools are not indexed, D5/D12).
    fn refresh_index_for_server(
        &self,
        name: &str,
        config: Option<&McpServerConfig>,
        fresh: &[McpToolDef],
    ) {
        let mut index = self
            .state
            .tool_index
            .lock()
            .expect("mcp tool index mutex poisoned");
        index.retain(|entry| entry.server != name);
        let mut taken: HashSet<String> = index.iter().map(|entry| entry.id.clone()).collect();
        for tool in fresh {
            let mode = match config {
                Some(cfg) => resolve_mode(cfg, &tool.name, tool.annotations.as_ref()),
                None => ToolMode::Deny,
            };
            if mode == ToolMode::Deny {
                continue;
            }
            let id = namespaced_tool_id(name, &tool.name, &taken);
            taken.insert(id.clone());
            index.push(McpToolEntry {
                id,
                server: name.to_string(),
                name: tool.name.clone(),
                description: tool.description.clone(),
                schema_digest: schema_digest(&tool.input_schema),
                input_schema: tool.input_schema.clone(),
            });
        }
    }
}

/// Gracefully close a pooled connection when this is its sole owner, else drop
/// it (the last `Arc` to drop group-SIGKILLs + reaps the stdio child). Used by
/// the Phase 6 teardown paths, which only hold a shared `Arc`.
async fn close_or_drop(connection: Arc<McpConnection>) {
    match Arc::try_unwrap(connection) {
        Ok(owned) => {
            if let Err(err) = owned.close().await {
                tracing::warn!(
                    target: "pocopine.log",
                    error = %err,
                    "mcp connection close failed during teardown"
                );
            }
        }
        // Still shared (an adapter / in-flight verb holds a clone): drop our
        // reference; the child is reaped when the last owner drops.
        Err(_shared) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_server_names() {
        let err = McpRuntime::new(vec![
            McpServerConfig::stdio("dup", "a"),
            McpServerConfig::stdio("dup", "b"),
        ])
        .unwrap_err();
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn rejects_blank_server_name() {
        let err = McpRuntime::new(vec![McpServerConfig::stdio("  ", "a")]).unwrap_err();
        assert_eq!(err.kind(), "config");
    }

    #[test]
    fn looks_up_configured_servers() {
        let runtime =
            McpRuntime::new(vec![McpServerConfig::stdio("github", "github-mcp")]).unwrap();
        assert_eq!(runtime.servers().len(), 1);
        assert!(runtime.server("github").is_some());
        assert!(runtime.server("missing").is_none());
    }

    #[test]
    fn empty_runtime_has_no_servers() {
        assert!(McpRuntime::empty().servers().is_empty());
    }

    #[test]
    fn fork_shares_config_but_has_fresh_state() {
        // A fork re-derives from config: same servers, but an independent
        // (empty) pool/index/pin store — no live transport is carried over.
        let runtime = McpRuntime::new(vec![McpServerConfig::stdio("s", "c")])
            .unwrap()
            .with_root("/tmp/root");
        runtime.publish_tool_index(vec![McpToolEntry {
            id: "mcp.s.t".to_string(),
            server: "s".to_string(),
            name: "t".to_string(),
            description: None,
            schema_digest: "d".to_string(),
            input_schema: serde_json::Map::new(),
        }]);
        runtime.pins().approve("s", "t", "hash-a");

        let forked = runtime.fork();
        // Config is shared…
        assert_eq!(forked.servers().len(), 1);
        assert_eq!(forked.root(), runtime.root());
        // …but the live state is fresh: empty index + a new, unapproved pin store.
        assert!(forked.tool_index().is_empty());
        assert!(!forked.pins().is_approved("s", "t", "hash-a"));
        assert!(!runtime.tool_index().is_empty(), "the source is untouched");
    }
}
