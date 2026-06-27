//! The gateway hub: configuration, topic authorization/resolver, and shared
//! state (RFC 073 §10–§11).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use pocopine_core::server::RequestContext;
use pocopine_events::Topic;

use super::error::WsError;
use super::fanout::{Fanout, LocalFanout};
use super::handler::SubprotocolHandler;

/// Default heartbeat interval the server asks clients to use.
const DEFAULT_HEARTBEAT_MS: u32 = 15_000;
/// Default number of missed heartbeat intervals before a connection is a
/// zombie.
const DEFAULT_ZOMBIE_GRACE: u32 = 3;
/// Default maximum size of a single inbound frame (1 MiB).
const DEFAULT_MAX_FRAME_BYTES: usize = 1 << 20;
/// Default bounded outbound queue depth per connection.
const DEFAULT_OUTBOUND_QUEUE: usize = 256;
/// Default cap on concurrent topic subscriptions per connection.
const DEFAULT_MAX_SUBSCRIPTIONS: usize = 256;
/// Default cap on a topic-name string (bytes).
const DEFAULT_MAX_TOPIC_BYTES: usize = 512;

/// What a connection may do on a topic — the whole authorization outcome of one
/// [`TopicAuthorizer`] decision.
///
/// `ReadWrite` permits join + publish; `ReadOnly` permits join only; `Denied`
/// permits neither. Write-without-join is unrepresentable.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TopicAccess {
    /// May neither subscribe nor publish.
    #[default]
    Denied,
    /// May subscribe (read) but not publish.
    ReadOnly,
    /// May subscribe and publish.
    ReadWrite,
}

impl TopicAccess {
    /// Whether the connection may subscribe to the topic.
    pub fn can_join(self) -> bool {
        !matches!(self, Self::Denied)
    }

    /// Whether the connection may publish to the topic.
    pub fn can_write(self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    /// Downgrade `ReadWrite` to `ReadOnly` when `writable` is false; otherwise
    /// unchanged. Used by the [`WsGateway::with_write_policy`] clamp.
    fn clamp_write(self, writable: bool) -> Self {
        match self {
            Self::ReadWrite if !writable => Self::ReadOnly,
            other => other,
        }
    }
}

/// Decides what a connection may do on a topic — join (read) AND publish (write)
/// in one **async** decision, so the check can hit a store (e.g. a
/// channel-membership lookup). The single authorization seam on the
/// [`WsGateway`]: it is awaited at subscribe time and re-checked on every inbound
/// frame (RFC 073 §10.1).
///
/// Most apps don't implement this directly — the closure builders
/// ([`WsGateway::with_access`], [`WsGateway::with_async_access`]) and the
/// back-compat [`WsGateway::with_topic_policy`] cover the common cases. Implement
/// it for a stateful, testable authorizer (one that carries a DB pool, a cache,
/// or capability tokens).
pub trait TopicAuthorizer: Send + Sync + 'static {
    fn authorize<'a>(
        &'a self,
        ctx: &'a RequestContext,
        topic: &'a Topic,
    ) -> Pin<Box<dyn Future<Output = TopicAccess> + Send + 'a>>;
}

/// Back-compat alias for the synchronous bool join-policy closure shape.
pub type TopicPolicy = Arc<dyn Fn(&RequestContext, &Topic) -> bool + Send + Sync>;

/// Resolves a client-supplied topic string to a canonical `Topic`.
pub type TopicResolver = Arc<dyn Fn(&str) -> Result<Topic, WsError> + Send + Sync>;

/// Adapts a synchronous capability closure into a [`TopicAuthorizer`].
struct FnAuthorizer<F>(F);

impl<F> TopicAuthorizer for FnAuthorizer<F>
where
    F: Fn(&RequestContext, &Topic) -> TopicAccess + Send + Sync + 'static,
{
    fn authorize<'a>(
        &'a self,
        ctx: &'a RequestContext,
        topic: &'a Topic,
    ) -> Pin<Box<dyn Future<Output = TopicAccess> + Send + 'a>> {
        let access = (self.0)(ctx, topic);
        Box::pin(std::future::ready(access))
    }
}

/// Adapts an async capability closure into a [`TopicAuthorizer`].
struct AsyncFnAuthorizer<F>(F);

impl<F, Fut> TopicAuthorizer for AsyncFnAuthorizer<F>
where
    F: Fn(RequestContext, Topic) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = TopicAccess> + Send + 'static,
{
    fn authorize<'a>(
        &'a self,
        ctx: &'a RequestContext,
        topic: &'a Topic,
    ) -> Pin<Box<dyn Future<Output = TopicAccess> + Send + 'a>> {
        Box::pin((self.0)(ctx.clone(), topic.clone()))
    }
}

/// Wraps an authorizer with a synchronous **write clamp**: a `ReadWrite` decision
/// is downgraded to `ReadOnly` when `write` returns false. Back-compat seam for
/// [`WsGateway::with_write_policy`].
struct WriteClamp {
    inner: Arc<dyn TopicAuthorizer>,
    write: TopicPolicy,
}

impl TopicAuthorizer for WriteClamp {
    fn authorize<'a>(
        &'a self,
        ctx: &'a RequestContext,
        topic: &'a Topic,
    ) -> Pin<Box<dyn Future<Output = TopicAccess> + Send + 'a>> {
        Box::pin(async move {
            let access = self.inner.authorize(ctx, topic).await;
            access.clamp_write((self.write)(ctx, topic))
        })
    }
}

/// Tunable connection limits and liveness timings.
#[derive(Clone, Copy, Debug)]
pub struct GatewayConfig {
    /// Interval the server asks clients to heartbeat at.
    pub heartbeat_interval_ms: u32,
    /// Missed heartbeat intervals tolerated before the connection is closed.
    pub zombie_grace: u32,
    /// Hard cap on a single inbound frame; larger frames are rejected.
    pub max_frame_bytes: usize,
    /// Bounded per-connection outbound queue depth (backpressure).
    pub outbound_queue: usize,
    /// Cap on concurrent topic subscriptions per connection.
    pub max_subscriptions: usize,
    /// Cap on a topic-name string length, in bytes.
    pub max_topic_bytes: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_ms: DEFAULT_HEARTBEAT_MS,
            zombie_grace: DEFAULT_ZOMBIE_GRACE,
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            outbound_queue: DEFAULT_OUTBOUND_QUEUE,
            max_subscriptions: DEFAULT_MAX_SUBSCRIPTIONS,
            max_topic_bytes: DEFAULT_MAX_TOPIC_BYTES,
        }
    }
}

/// The mountable gateway. Clone-cheap (all shared state is behind `Arc`), as
/// axum 0.7 requires router state to be `Clone`.
#[derive(Clone)]
pub struct WsGateway {
    fanout: Arc<dyn Fanout>,
    /// The single join + publish authorization seam. Defaults to deny-all.
    authorizer: Arc<dyn TopicAuthorizer>,
    resolver: TopicResolver,
    config: GatewayConfig,
    sessions: Arc<AtomicU64>,
    /// Stateful inbound handlers keyed by `subprotocol_id`. A sub-protocol with
    /// no entry uses the default publish-to-fan-out relay.
    handlers: Arc<HashMap<u64, Arc<dyn SubprotocolHandler>>>,
    /// Live subscription count per `(topic, subprotocol_id)` across all of this
    /// process's connections. Keyed by sub-protocol too, so the active/idle
    /// lifecycle (0↔1) always reaches the handler that owns that sub-protocol.
    subscriber_counts: Arc<Mutex<HashMap<(Topic, u64), usize>>>,
}

impl WsGateway {
    /// Build a gateway over an explicit [`Fanout`].
    ///
    /// The default authorizer denies every topic (matching `LiveHub`'s safe
    /// default); open it with [`WsGateway::allow_all_topics`],
    /// [`WsGateway::allow_topics`], [`WsGateway::with_access`],
    /// [`WsGateway::with_async_access`], [`WsGateway::with_authorizer`], or the
    /// back-compat [`WsGateway::with_topic_policy`]. The default resolver maps a
    /// topic string straight to a `Topic`.
    pub fn new(fanout: Arc<dyn Fanout>) -> Self {
        Self {
            fanout,
            authorizer: Arc::new(FnAuthorizer(|_: &RequestContext, _: &Topic| {
                TopicAccess::Denied
            })),
            resolver: Arc::new(|topic| Topic::new(topic).map_err(WsError::from)),
            config: GatewayConfig::default(),
            sessions: Arc::new(AtomicU64::new(0)),
            handlers: Arc::new(HashMap::new()),
            subscriber_counts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Build a gateway backed by the in-process [`LocalFanout`] (single
    /// process / tests).
    pub fn local() -> Self {
        Self::new(Arc::new(LocalFanout::new()))
    }

    /// Build a gateway backed by Redis for multi-process fan-out (RFC 073
    /// Phase C). Requires the `redis` feature.
    #[cfg(feature = "redis")]
    pub async fn redis(url: &str, app: impl Into<String>) -> Result<Self, WsError> {
        Ok(Self::new(Arc::new(
            super::redis_fanout::RedisFanout::connect(url, app).await?,
        )))
    }

    /// Build a Redis-backed gateway, reading `POCOPINE_REDIS_URL`. Requires the
    /// `redis` feature.
    #[cfg(feature = "redis")]
    pub async fn redis_from_env(app: impl Into<String>) -> Result<Self, WsError> {
        Ok(Self::new(Arc::new(
            super::redis_fanout::RedisFanout::from_env(app).await?,
        )))
    }

    /// Set the [`TopicAuthorizer`] — the join + publish decision for every topic.
    /// Replaces any previously-set authorizer / policy.
    pub fn with_authorizer(mut self, authorizer: impl TopicAuthorizer) -> Self {
        self.authorizer = Arc::new(authorizer);
        self
    }

    /// Set a **synchronous** capability policy: one closure returns the full
    /// [`TopicAccess`] (join + write) for a connection/topic.
    pub fn with_access(
        self,
        policy: impl Fn(&RequestContext, &Topic) -> TopicAccess + Send + Sync + 'static,
    ) -> Self {
        self.with_authorizer(FnAuthorizer(policy))
    }

    /// Set an **async** capability policy — the closure awaits (e.g. a DB
    /// membership lookup) and returns the full [`TopicAccess`]. The closure
    /// receives an owned [`RequestContext`] (it carries the principal) + the
    /// resolved [`Topic`].
    pub fn with_async_access<F, Fut>(self, policy: F) -> Self
    where
        F: Fn(RequestContext, Topic) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = TopicAccess> + Send + 'static,
    {
        self.with_authorizer(AsyncFnAuthorizer(policy))
    }

    /// Replace the per-topic join policy (back-compat bool shape): `true` →
    /// [`TopicAccess::ReadWrite`], `false` → [`TopicAccess::Denied`]. Prefer
    /// [`with_access`](Self::with_access) / [`with_async_access`](Self::with_async_access)
    /// to express read-only access or an async lookup directly.
    pub fn with_topic_policy(
        self,
        policy: impl Fn(&RequestContext, &Topic) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.with_access(move |ctx, topic| {
            if policy(ctx, topic) {
                TopicAccess::ReadWrite
            } else {
                TopicAccess::Denied
            }
        })
    }

    /// Layer a **publish** restriction on top of the current authorizer: when
    /// `policy` returns false a `ReadWrite` decision is clamped to `ReadOnly` (the
    /// connection may join but not publish), and handlers see
    /// [`InboundData::can_write`](super::handler::InboundData) `= false`. Prefer
    /// returning [`TopicAccess::ReadOnly`] from [`with_access`](Self::with_access).
    pub fn with_write_policy(
        mut self,
        policy: impl Fn(&RequestContext, &Topic) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.authorizer = Arc::new(WriteClamp {
            inner: self.authorizer.clone(),
            write: Arc::new(policy),
        });
        self
    }

    /// Allow any connection to join and publish on any topic. Use only when topic
    /// access is enforced elsewhere or is genuinely public.
    pub fn allow_all_topics(self) -> Self {
        self.with_access(|_, _| TopicAccess::ReadWrite)
    }

    /// Allow only the given set of `Topic`s (join + publish); deny the rest.
    pub fn allow_topics(self, topics: impl IntoIterator<Item = Topic>) -> Self {
        let allowed: Arc<Vec<Topic>> = Arc::new(topics.into_iter().collect());
        self.with_access(move |_, topic| {
            if allowed.iter().any(|t| t == topic) {
                TopicAccess::ReadWrite
            } else {
                TopicAccess::Denied
            }
        })
    }

    /// Replace the topic-string → `Topic` resolver.
    pub fn with_resolver(
        mut self,
        resolver: impl Fn(&str) -> Result<Topic, WsError> + Send + Sync + 'static,
    ) -> Self {
        self.resolver = Arc::new(resolver);
        self
    }

    /// Override connection limits / liveness timings.
    pub fn with_config(mut self, config: GatewayConfig) -> Self {
        self.config = config;
        self
    }

    /// Register a stateful inbound [`SubprotocolHandler`] for `subprotocol_id`.
    ///
    /// Data frames tagged with that id are routed to the handler instead of the
    /// default publish-to-fan-out relay; every other sub-protocol still relays.
    /// Registering twice for the same id replaces the earlier handler.
    pub fn with_handler(
        mut self,
        subprotocol_id: u64,
        handler: Arc<dyn SubprotocolHandler>,
    ) -> Self {
        // Uniquely owned during the builder chain, so this never deep-clones.
        Arc::make_mut(&mut self.handlers).insert(subprotocol_id, handler);
        self
    }

    /// The [`Fanout`] this gateway publishes to. Exposed so a sub-protocol that
    /// also folds the fan-out itself — notably `pocopine-collab`'s convergence
    /// apply loop — can be wired to the SAME instance, which is the one
    /// correctness requirement for multi-process convergence. Prefer the
    /// `WsGateway::with_collab*` helpers (in `pocopine-collab`), which use this
    /// to make that wiring correct by construction.
    pub fn fanout(&self) -> &Arc<dyn Fanout> {
        &self.fanout
    }

    pub(crate) fn config(&self) -> GatewayConfig {
        self.config
    }

    pub(crate) fn resolve(&self, topic: &str) -> Result<Topic, WsError> {
        (self.resolver)(topic)
    }

    /// Authorize a connection against a topic — the single join + publish
    /// decision, awaiting the [`TopicAuthorizer`]. Checked at subscribe time and
    /// re-checked on every inbound frame.
    pub(crate) async fn authorize(&self, ctx: &RequestContext, topic: &Topic) -> TopicAccess {
        self.authorizer.authorize(ctx, topic).await
    }

    pub(crate) fn handler(&self, subprotocol_id: u64) -> Option<&Arc<dyn SubprotocolHandler>> {
        self.handlers.get(&subprotocol_id)
    }

    /// Record a new subscription to `(topic, subprotocol_id)`; notify the
    /// sub-protocol's handler if this is its first live subscriber on this
    /// process (0→1).
    ///
    /// The handler is notified WHILE the count lock is held: the lock orders the
    /// 0→1 / 1→0 transitions, and notifying inside it makes the active/idle
    /// callbacks arrive in that same order. Otherwise a concurrent unsubscribe
    /// (1→0) and subscribe (0→1) on one topic could deliver idle-after-active
    /// and leave a live topic with no handler state. (Lock order is always
    /// counts → handler's own lock; nothing takes them the other way, so no
    /// deadlock.)
    pub(crate) fn topic_subscribed(&self, topic: &Topic, subprotocol_id: u64) {
        let mut counts = self.subscriber_counts.lock().expect("counts poisoned");
        let n = counts.entry((topic.clone(), subprotocol_id)).or_insert(0);
        *n += 1;
        if *n == 1
            && let Some(handler) = self.handler(subprotocol_id)
        {
            handler.on_topic_active(topic);
        }
    }

    /// Record a dropped subscription to `(topic, subprotocol_id)`; notify the
    /// sub-protocol's handler (under the lock) if that was its last live
    /// subscriber on this process (1→0). Idempotent for an unknown key.
    pub(crate) fn topic_unsubscribed(&self, topic: &Topic, subprotocol_id: u64) {
        let mut counts = self.subscriber_counts.lock().expect("counts poisoned");
        let key = (topic.clone(), subprotocol_id);
        let drained = match counts.get_mut(&key) {
            Some(n) => {
                *n = n.saturating_sub(1);
                *n == 0
            }
            None => return,
        };
        if drained {
            counts.remove(&key);
            if let Some(handler) = self.handler(subprotocol_id) {
                handler.on_topic_idle(topic);
            }
        }
    }

    pub(crate) fn next_session_id(&self) -> String {
        let n = self.sessions.fetch_add(1, Ordering::Relaxed);
        format!("ws-{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_core::server::RequestContext;

    fn anon_ctx() -> RequestContext {
        RequestContext::new(
            axum::http::Method::GET,
            "/__pocopine/ws/v1".parse().unwrap(),
            axum::http::HeaderMap::new(),
        )
    }

    #[tokio::test]
    async fn default_authorizer_denies_all_topics() {
        let gateway = WsGateway::local();
        let topic = Topic::new("collab:abc").unwrap();
        assert_eq!(
            gateway.authorize(&anon_ctx(), &topic).await,
            TopicAccess::Denied
        );
    }

    #[tokio::test]
    async fn allow_topics_permits_only_listed_read_write() {
        let allowed = Topic::new("collab:abc").unwrap();
        let other = Topic::new("collab:xyz").unwrap();
        let gateway = WsGateway::local().allow_topics([allowed.clone()]);
        assert_eq!(
            gateway.authorize(&anon_ctx(), &allowed).await,
            TopicAccess::ReadWrite
        );
        assert_eq!(
            gateway.authorize(&anon_ctx(), &other).await,
            TopicAccess::Denied
        );
    }

    #[tokio::test]
    async fn write_policy_clamps_read_write_to_read_only() {
        let topic = Topic::new("collab:abc").unwrap();
        let gateway = WsGateway::local()
            .allow_all_topics()
            .with_write_policy(|_, _| false);
        let access = gateway.authorize(&anon_ctx(), &topic).await;
        assert!(access.can_join());
        assert!(!access.can_write());
        assert_eq!(access, TopicAccess::ReadOnly);
    }

    #[tokio::test]
    async fn async_authorizer_decides_per_topic() {
        let allowed = Topic::new("chat:abc").unwrap();
        let denied = Topic::new("chat:xyz").unwrap();
        // The authorizer can await (here a trivial future) and decide per topic.
        let gateway = WsGateway::local().with_async_access(|_ctx, topic: Topic| async move {
            if topic.as_str() == "chat:abc" {
                TopicAccess::ReadWrite
            } else {
                TopicAccess::Denied
            }
        });
        assert_eq!(
            gateway.authorize(&anon_ctx(), &allowed).await,
            TopicAccess::ReadWrite
        );
        assert_eq!(
            gateway.authorize(&anon_ctx(), &denied).await,
            TopicAccess::Denied
        );
    }

    #[tokio::test]
    async fn with_access_expresses_read_only() {
        let topic = Topic::new("chat:abc").unwrap();
        let gateway = WsGateway::local().with_access(|_, _| TopicAccess::ReadOnly);
        let access = gateway.authorize(&anon_ctx(), &topic).await;
        assert!(access.can_join());
        assert!(!access.can_write());
    }

    #[test]
    fn session_ids_are_unique() {
        let gateway = WsGateway::local();
        assert_ne!(gateway.next_session_id(), gateway.next_session_id());
    }

    #[test]
    fn default_resolver_rejects_blank_topic() {
        let gateway = WsGateway::local();
        assert!(gateway.resolve("").is_err());
        assert!(gateway.resolve("collab:abc").is_ok());
    }
}
