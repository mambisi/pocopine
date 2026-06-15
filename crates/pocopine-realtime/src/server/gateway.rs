//! The gateway hub: configuration, topic policy/resolver, and shared state
//! (RFC 073 §10–§11).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use pocopine_auth::RequestContext;
use pocopine_events::Topic;

use super::error::WsError;
use super::fanout::{Fanout, LocalFanout};

/// WebSocket sub-protocol identifier advertised in `Sec-WebSocket-Protocol`
/// and the [`crate::Control::Hello`] frame.
pub const WS_PROTOCOL_V1: &str = "pocopine.ws.v1";

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

/// Per-topic authorization policy: may this `RequestContext` join this `Topic`?
///
/// Mirrors `LiveHub::with_topic_policy`'s synchronous bool shape (RFC 073
/// §10.1). Capability-bearing consumers (e.g. collab read-only vs read-write)
/// layer a richer async authorizer on top in their own crates.
pub type TopicPolicy = Arc<dyn Fn(&RequestContext, &Topic) -> bool + Send + Sync>;

/// Resolves a client-supplied topic string to a canonical `Topic`.
pub type TopicResolver = Arc<dyn Fn(&str) -> Result<Topic, WsError> + Send + Sync>;

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
    policy: TopicPolicy,
    resolver: TopicResolver,
    config: GatewayConfig,
    sessions: Arc<AtomicU64>,
}

impl WsGateway {
    /// Build a gateway over an explicit [`Fanout`].
    ///
    /// The default topic policy denies every topic (matching `LiveHub`'s safe
    /// default); open it with [`WsGateway::allow_all_topics`],
    /// [`WsGateway::allow_topics`], or [`WsGateway::with_topic_policy`]. The
    /// default resolver maps a topic string straight to a `Topic`.
    pub fn new(fanout: Arc<dyn Fanout>) -> Self {
        Self {
            fanout,
            policy: Arc::new(|_, _| false),
            resolver: Arc::new(|topic| Topic::new(topic).map_err(WsError::from)),
            config: GatewayConfig::default(),
            sessions: Arc::new(AtomicU64::new(0)),
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

    /// Replace the per-topic authorization policy.
    pub fn with_topic_policy(
        mut self,
        policy: impl Fn(&RequestContext, &Topic) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.policy = Arc::new(policy);
        self
    }

    /// Allow any connection to join any topic. Use only when topic access is
    /// enforced elsewhere or is genuinely public.
    pub fn allow_all_topics(self) -> Self {
        self.with_topic_policy(|_, _| true)
    }

    /// Allow only the given set of `Topic`s.
    pub fn allow_topics(self, topics: impl IntoIterator<Item = Topic>) -> Self {
        let allowed: Arc<Vec<Topic>> = Arc::new(topics.into_iter().collect());
        self.with_topic_policy(move |_, topic| allowed.iter().any(|t| t == topic))
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

    // --- internal accessors used by the route/session machinery ---

    pub(crate) fn fanout(&self) -> &Arc<dyn Fanout> {
        &self.fanout
    }

    pub(crate) fn config(&self) -> GatewayConfig {
        self.config
    }

    pub(crate) fn resolve(&self, topic: &str) -> Result<Topic, WsError> {
        (self.resolver)(topic)
    }

    pub(crate) fn authorize(&self, ctx: &RequestContext, topic: &Topic) -> bool {
        (self.policy)(ctx, topic)
    }

    pub(crate) fn next_session_id(&self) -> String {
        let n = self.sessions.fetch_add(1, Ordering::Relaxed);
        format!("ws-{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocopine_auth::RequestContext;

    fn anon_ctx() -> RequestContext {
        RequestContext::new(
            axum::http::Method::GET,
            "/__pocopine/ws/v1".parse().unwrap(),
            axum::http::HeaderMap::new(),
        )
    }

    #[test]
    fn default_policy_denies_all_topics() {
        let gateway = WsGateway::local();
        let topic = Topic::new("collab:abc").unwrap();
        assert!(!gateway.authorize(&anon_ctx(), &topic));
    }

    #[test]
    fn allow_topics_permits_only_listed() {
        let allowed = Topic::new("collab:abc").unwrap();
        let other = Topic::new("collab:xyz").unwrap();
        let gateway = WsGateway::local().allow_topics([allowed.clone()]);
        assert!(gateway.authorize(&anon_ctx(), &allowed));
        assert!(!gateway.authorize(&anon_ctx(), &other));
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
