//! Event spine contracts for pocopine live, sync, and collaboration
//! integrations.
//!
//! The crate owns neutral event envelopes, opaque cursors, topics, and
//! backend traits. Runtime crates can build live streams or sync
//! protocols on top of these contracts without exposing raw database,
//! Redis, or CDC topics to browsers.

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current neutral event-envelope protocol identifier.
pub const EVENT_PROTOCOL_V1: &str = "pocopine.events.v1";

/// Future returned by an event backend operation.
pub type EventFuture<'a, T> = Pin<Box<dyn Future<Output = EventResult<T>> + Send + 'a>>;

/// Canonical result type for the event spine.
pub type EventResult<T> = Result<T, EventError>;

/// Built-in backend family advertised by an [`EventBackend`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventBackendKind {
    /// Process-local bounded replay and wake-up buffers.
    Memory,
    /// Redis Streams for replay with Pub/Sub wake-ups.
    Redis,
    /// Application-provided backend.
    Custom,
}

/// Capability metadata for an event backend.
///
/// Pocopine live/sync layers use this to choose safe defaults without
/// encoding a concrete broker type in the browser protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EventBackendCapabilities {
    /// Backend family.
    pub kind: EventBackendKind,
    /// Whether replay data survives process restarts.
    pub durable_replay: bool,
    /// Whether subscribers can receive future-event wake-ups without polling.
    pub live_wakeups: bool,
    /// Whether the backend can coordinate multiple server processes.
    pub multi_process: bool,
    /// Whether a topic's events are replayed in stable publish order.
    pub ordered_per_topic: bool,
    /// Whether opaque cursors can resume after disconnect.
    pub cursor_resume: bool,
}

impl EventBackendCapabilities {
    /// Capability set for an unknown application backend.
    pub const fn custom() -> Self {
        Self {
            kind: EventBackendKind::Custom,
            durable_replay: false,
            live_wakeups: false,
            multi_process: false,
            ordered_per_topic: false,
            cursor_resume: false,
        }
    }

    /// Capability set for [`MemoryEventBackend`].
    pub const fn memory() -> Self {
        Self {
            kind: EventBackendKind::Memory,
            durable_replay: false,
            live_wakeups: true,
            multi_process: false,
            ordered_per_topic: true,
            cursor_resume: true,
        }
    }

    /// Capability set for the Redis Streams/PubSub backend.
    pub const fn redis_streams() -> Self {
        Self {
            kind: EventBackendKind::Redis,
            durable_replay: true,
            live_wakeups: true,
            multi_process: true,
            ordered_per_topic: true,
            cursor_resume: true,
        }
    }
}

/// Errors produced by event validation and backend operations.
#[derive(Debug)]
pub enum EventError {
    /// A topic, kind, cursor, or id was empty or malformed.
    InvalidValue { field: &'static str, value: String },
    /// A replay cursor is too old for the configured retention window.
    Gap { cursor: EventCursor },
    /// A live subscriber fell behind the backend's wake-up buffer.
    SubscriptionLagged { skipped: u64 },
    /// Backend retention was configured to an unusable value.
    InvalidRetention(String),
    /// JSON payload or metadata could not be encoded or decoded.
    Json(serde_json::Error),
    /// A backend lock was poisoned.
    Backend(String),
    /// Redis returned an error.
    #[cfg(all(feature = "redis", not(target_arch = "wasm32")))]
    Redis(redis::RedisError),
    /// System time moved before Unix epoch.
    Time(String),
    /// The requested backend is unavailable on this target.
    Unsupported(String),
}

impl EventError {
    fn invalid_value(field: &'static str, value: impl Into<String>) -> Self {
        Self::InvalidValue {
            field,
            value: value.into(),
        }
    }

    /// Build a host-only unsupported failure.
    pub fn unsupported(msg: impl Into<String>) -> Self {
        Self::Unsupported(msg.into())
    }
}

impl fmt::Display for EventError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { field, value } => {
                write!(f, "invalid {field}: {value:?}")
            }
            Self::Gap { cursor } => write!(f, "event cursor is no longer replayable: {cursor}"),
            Self::SubscriptionLagged { skipped } => {
                write!(f, "event subscription lagged by {skipped} event(s)")
            }
            Self::InvalidRetention(msg) => write!(f, "invalid event retention: {msg}"),
            Self::Json(err) => write!(f, "event json error: {err}"),
            Self::Backend(msg) => write!(f, "event backend error: {msg}"),
            #[cfg(all(feature = "redis", not(target_arch = "wasm32")))]
            Self::Redis(err) => write!(f, "redis event backend error: {err}"),
            Self::Time(msg) => write!(f, "time error: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for EventError {}

impl From<serde_json::Error> for EventError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

#[cfg(all(feature = "redis", not(target_arch = "wasm32")))]
impl From<redis::RedisError> for EventError {
    fn from(err: redis::RedisError) -> Self {
        Self::Redis(err)
    }
}

fn validate_non_empty(field: &'static str, value: String) -> EventResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(EventError::invalid_value(field, value));
    }
    if trimmed != value {
        return Err(EventError::invalid_value(field, value));
    }
    Ok(value)
}

fn validate_no_control_chars(field: &'static str, value: &str) -> EventResult<()> {
    if value.chars().any(char::is_control) {
        return Err(EventError::invalid_value(field, value));
    }
    Ok(())
}

macro_rules! opaque_string_type {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Build a validated value.
            pub fn new(value: impl Into<String>) -> EventResult<Self> {
                let value = validate_non_empty($field, value.into())?;
                validate_no_control_chars($field, &value)?;
                Ok(Self(value))
            }

            /// Borrow the stable string value.
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl TryFrom<&str> for $name {
            type Error = EventError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = EventError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }
    };
}

opaque_string_type!(
    EventId,
    "event id",
    "Opaque id assigned to a published event."
);
opaque_string_type!(
    EventCursor,
    "event cursor",
    "Opaque backend cursor used for replay and resume."
);
opaque_string_type!(
    EventKind,
    "event kind",
    "Stable event kind such as `collection.changed` or `query.invalidated`."
);
opaque_string_type!(
    Topic,
    "event topic",
    "Framework topic name. Browsers should see application topics, not raw backend keys."
);

/// Convert a value into a validated [`Topic`].
pub trait IntoTopic {
    /// Perform the conversion.
    fn into_topic(self) -> EventResult<Topic>;
}

impl IntoTopic for Topic {
    fn into_topic(self) -> EventResult<Topic> {
        Ok(self)
    }
}

impl IntoTopic for &str {
    fn into_topic(self) -> EventResult<Topic> {
        Topic::new(self)
    }
}

impl IntoTopic for String {
    fn into_topic(self) -> EventResult<Topic> {
        Topic::new(self)
    }
}

/// Convert a value into a validated [`EventKind`].
pub trait IntoEventKind {
    /// Perform the conversion.
    fn into_event_kind(self) -> EventResult<EventKind>;
}

impl IntoEventKind for EventKind {
    fn into_event_kind(self) -> EventResult<EventKind> {
        Ok(self)
    }
}

impl IntoEventKind for &str {
    fn into_event_kind(self) -> EventResult<EventKind> {
        EventKind::new(self)
    }
}

impl IntoEventKind for String {
    fn into_event_kind(self) -> EventResult<EventKind> {
        EventKind::new(self)
    }
}

/// Audience metadata attached to an event.
///
/// This is descriptive metadata for downstream filtering. It is not an
/// authorization decision by itself; live/sync layers must still check
/// their own guards before delivering an event.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Audience {
    /// Event may be delivered to anonymous users if the topic policy
    /// allows it.
    Public,
    /// Event requires an authenticated principal.
    #[default]
    Authenticated,
    /// Event is intended for one user id.
    User { id: String },
    /// Event is intended for any of the listed user ids.
    Users { ids: Vec<String> },
    /// Event is intended for one role name.
    Role { name: String },
    /// Event is intended for any of the listed role names.
    Roles { names: Vec<String> },
    /// App-specific audience label.
    Custom { name: String },
}

/// Draft event supplied by publishers before a backend assigns id/cursor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventDraft {
    /// Envelope protocol identifier.
    pub protocol: String,
    /// Framework topic.
    pub topic: Topic,
    /// Stable event kind.
    pub kind: EventKind,
    /// Descriptive audience metadata.
    pub audience: Audience,
    /// Event payload. Payload contents are topic-specific and should be
    /// redacted by emitters before publication.
    pub payload: Value,
    /// Milliseconds since Unix epoch. A value of `0` means the backend
    /// should fill its current clock when it can.
    pub created_at_ms: u64,
    /// Topic schema version.
    pub schema_version: u32,
}

impl EventDraft {
    /// Build a draft with the default event protocol, authenticated
    /// audience, and schema version 1.
    pub fn new(
        topic: impl IntoTopic,
        kind: impl IntoEventKind,
        payload: impl Into<Value>,
    ) -> EventResult<Self> {
        Ok(Self {
            protocol: EVENT_PROTOCOL_V1.to_string(),
            topic: topic.into_topic()?,
            kind: kind.into_event_kind()?,
            audience: Audience::default(),
            payload: payload.into(),
            created_at_ms: 0,
            schema_version: 1,
        })
    }

    /// Set audience metadata.
    pub fn audience(mut self, audience: Audience) -> Self {
        self.audience = audience;
        self
    }

    /// Set the event timestamp in milliseconds since Unix epoch.
    pub fn created_at_ms(mut self, created_at_ms: u64) -> Self {
        self.created_at_ms = created_at_ms;
        self
    }

    /// Set the topic schema version.
    pub fn schema_version(mut self, schema_version: u32) -> Self {
        self.schema_version = schema_version;
        self
    }
}

/// Published event with backend-assigned id and cursor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Envelope protocol identifier.
    pub protocol: String,
    /// Opaque event id.
    pub id: EventId,
    /// Framework topic.
    pub topic: Topic,
    /// Stable event kind.
    pub kind: EventKind,
    /// Descriptive audience metadata.
    pub audience: Audience,
    /// Opaque replay cursor.
    pub cursor: EventCursor,
    /// Event payload.
    pub payload: Value,
    /// Milliseconds since Unix epoch.
    pub created_at_ms: u64,
    /// Topic schema version.
    pub schema_version: u32,
}

impl EventEnvelope {
    /// Build an envelope from a draft and backend-assigned identity.
    pub fn from_draft(
        draft: EventDraft,
        id: EventId,
        cursor: EventCursor,
        created_at_ms: u64,
    ) -> Self {
        Self {
            protocol: draft.protocol,
            id,
            topic: draft.topic,
            kind: draft.kind,
            audience: draft.audience,
            cursor,
            payload: draft.payload,
            created_at_ms,
            schema_version: draft.schema_version,
        }
    }
}

/// Replay request for one or more topics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplayRequest {
    /// Topics to replay. An empty list is rejected by backends.
    pub topics: Vec<Topic>,
    /// Replay events strictly after this cursor. `None` asks for the
    /// retained snapshot for the topics.
    pub after: Option<EventCursor>,
    /// Maximum events to return.
    pub limit: usize,
}

impl ReplayRequest {
    /// Build a replay request.
    pub fn new(topics: impl IntoIterator<Item = Topic>) -> Self {
        Self {
            topics: topics.into_iter().collect(),
            after: None,
            limit: 100,
        }
    }

    /// Set the cursor to replay after.
    pub fn after(mut self, cursor: impl Into<Option<EventCursor>>) -> Self {
        self.after = cursor.into();
        self
    }

    /// Set the maximum number of events to return.
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// Replay result.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayBatch {
    /// Replayed events, ordered by backend cursor.
    pub events: Vec<EventEnvelope>,
    /// Last cursor in the returned batch, if any.
    pub cursor: Option<EventCursor>,
    /// Whether the requested cursor is older than backend retention.
    pub gap: bool,
}

impl ReplayBatch {
    /// Build a replay batch and infer the batch cursor from the last
    /// returned event.
    pub fn from_events(events: Vec<EventEnvelope>, gap: bool) -> Self {
        let cursor = events.last().map(|event| event.cursor.clone());
        Self {
            events,
            cursor,
            gap,
        }
    }
}

/// Subscription request. Phase A returns replay state only; live wake-up
/// streams are layered on top by `pocopine-live`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubscribeRequest {
    /// Topics requested by the caller.
    pub topics: Vec<Topic>,
    /// Optional resume cursor.
    pub after: Option<EventCursor>,
    /// Maximum replay events to include when opening the subscription.
    pub replay_limit: usize,
}

impl SubscribeRequest {
    /// Build a subscription request.
    pub fn new(topics: impl IntoIterator<Item = Topic>) -> Self {
        Self {
            topics: topics.into_iter().collect(),
            after: None,
            replay_limit: 100,
        }
    }

    /// Set a resume cursor.
    pub fn after(mut self, cursor: impl Into<Option<EventCursor>>) -> Self {
        self.after = cursor.into();
        self
    }

    /// Set replay limit used when opening the subscription.
    pub fn replay_limit(mut self, limit: usize) -> Self {
        self.replay_limit = limit;
        self
    }
}

/// Open subscription state.
#[derive(Clone, Debug, PartialEq)]
pub struct EventSubscription {
    /// Topics accepted by the backend.
    pub topics: Vec<Topic>,
    /// Replay batch returned while opening the subscription.
    pub replay: ReplayBatch,
}

/// Event backend abstraction.
pub trait EventBackend: Send + Sync {
    /// Advertise backend capabilities.
    fn capabilities(&self) -> EventBackendCapabilities {
        EventBackendCapabilities::custom()
    }

    /// Publish a draft event and return the stored envelope.
    fn publish<'a>(&'a self, draft: EventDraft) -> EventFuture<'a, EventEnvelope>;

    /// Replay retained events.
    fn replay<'a>(&'a self, request: ReplayRequest) -> EventFuture<'a, ReplayBatch>;

    /// Open a subscription. Phase A only promises replay state; live
    /// streaming can be added by backend-specific subscription handles.
    fn subscribe<'a>(&'a self, request: SubscribeRequest) -> EventFuture<'a, EventSubscription>;
}

/// Future returned by a live event receiver.
#[cfg(not(target_arch = "wasm32"))]
pub type EventReceiverFuture<'a> =
    Pin<Box<dyn Future<Output = EventResult<Option<EventEnvelope>>> + Send + 'a>>;

/// Receiver for future events after opening replay has completed.
#[cfg(not(target_arch = "wasm32"))]
pub trait EventReceiver: Send {
    /// Return the next future event, or `None` if the backend closed.
    fn next<'a>(&'a mut self) -> EventReceiverFuture<'a>;
}

/// Open live subscription with replay plus a future-event receiver.
#[cfg(not(target_arch = "wasm32"))]
pub struct LiveEventSubscription {
    /// Opening subscription state and replay.
    pub opening: EventSubscription,
    receiver: Box<dyn EventReceiver>,
}

#[cfg(not(target_arch = "wasm32"))]
impl LiveEventSubscription {
    /// Build a live subscription from opening replay and a receiver.
    pub fn new(opening: EventSubscription, receiver: Box<dyn EventReceiver>) -> Self {
        Self { opening, receiver }
    }

    /// Return the next future event, or `None` if the backend closed.
    pub async fn next(&mut self) -> EventResult<Option<EventEnvelope>> {
        self.receiver.next().await
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl fmt::Debug for LiveEventSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveEventSubscription")
            .field("opening", &self.opening)
            .finish_non_exhaustive()
    }
}

/// Host-side backend extension for live event streams.
#[cfg(not(target_arch = "wasm32"))]
pub trait LiveEventBackend: EventBackend {
    /// Open a subscription with replay plus a future-event receiver.
    fn subscribe_live<'a>(
        &'a self,
        request: SubscribeRequest,
    ) -> EventFuture<'a, LiveEventSubscription>;
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    use pocopine_observe::TRACE_TARGET;
    use tokio::sync::broadcast;

    use crate::{
        EventBackend, EventBackendCapabilities, EventCursor, EventDraft, EventEnvelope, EventError,
        EventFuture, EventId, EventReceiver, EventReceiverFuture, EventResult, EventSubscription,
        LiveEventBackend, LiveEventSubscription, ReplayBatch, ReplayRequest, SubscribeRequest,
        Topic,
    };

    /// In-memory event backend configuration.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct MemoryEventConfig {
        /// Maximum retained events.
        pub capacity: usize,
        /// Maximum future events buffered per live subscriber.
        pub subscriber_capacity: usize,
    }

    impl Default for MemoryEventConfig {
        fn default() -> Self {
            Self {
                capacity: 1_024,
                subscriber_capacity: 1_024,
            }
        }
    }

    #[derive(Debug)]
    struct MemoryEventState {
        next_seq: u64,
        events: VecDeque<EventEnvelope>,
    }

    impl Default for MemoryEventState {
        fn default() -> Self {
            Self {
                next_seq: 1,
                events: VecDeque::new(),
            }
        }
    }

    /// Process-local event backend with bounded replay.
    ///
    /// This backend is suitable for tests, development, and explicit
    /// single-process deployments. It is not shared across processes and
    /// does not survive restarts.
    #[derive(Clone, Debug)]
    pub struct MemoryEventBackend {
        config: MemoryEventConfig,
        state: Arc<Mutex<MemoryEventState>>,
        publisher: broadcast::Sender<EventEnvelope>,
    }

    impl MemoryEventBackend {
        /// Create an in-memory backend with default retention.
        pub fn new() -> Self {
            Self::with_config(MemoryEventConfig::default())
                .expect("default memory event retention must be valid")
        }

        /// Create an in-memory backend with a custom retained-event cap.
        pub fn with_capacity(capacity: usize) -> EventResult<Self> {
            Self::with_config(MemoryEventConfig {
                capacity,
                subscriber_capacity: capacity,
            })
        }

        /// Create an in-memory backend from config.
        pub fn with_config(config: MemoryEventConfig) -> EventResult<Self> {
            if config.capacity == 0 {
                return Err(EventError::InvalidRetention(
                    "capacity must be greater than zero".to_string(),
                ));
            }
            if config.subscriber_capacity == 0 {
                return Err(EventError::InvalidRetention(
                    "subscriber_capacity must be greater than zero".to_string(),
                ));
            }
            let (publisher, _) = broadcast::channel(config.subscriber_capacity);

            Ok(Self {
                config,
                state: Arc::new(Mutex::new(MemoryEventState::default())),
                publisher,
            })
        }

        /// Publish immediately without requiring an async executor.
        pub fn publish_now(&self, draft: EventDraft) -> EventResult<EventEnvelope> {
            let envelope = {
                let mut state = self.lock_state()?;
                let seq = state.next_seq;
                state.next_seq = state.next_seq.saturating_add(1);

                let id = EventId::new(format!("memory:{seq}"))?;
                let cursor = EventCursor::new(format!("memory:{seq}"))?;
                let created_at_ms = if draft.created_at_ms == 0 {
                    epoch_ms()?
                } else {
                    draft.created_at_ms
                };
                let envelope = EventEnvelope::from_draft(draft, id, cursor, created_at_ms);

                state.events.push_back(envelope.clone());
                while state.events.len() > self.config.capacity {
                    state.events.pop_front();
                }

                envelope
            };
            let subscribers = self.publisher.send(envelope.clone()).unwrap_or(0);

            tracing::debug!(
                target: TRACE_TARGET,
                event_name = "pocopine.events.memory.publish",
                topic = %envelope.topic,
                kind = %envelope.kind,
                cursor = %envelope.cursor,
                subscribers,
            );

            Ok(envelope)
        }

        /// Replay immediately without requiring an async executor.
        pub fn replay_now(&self, request: ReplayRequest) -> EventResult<ReplayBatch> {
            validate_replay_request(&request.topics, request.limit)?;
            let state = self.lock_state()?;
            let after_seq = request
                .after
                .as_ref()
                .map(parse_memory_cursor)
                .transpose()?;

            let oldest_seq = state
                .events
                .front()
                .map(|event| parse_memory_cursor(&event.cursor))
                .transpose()?;
            if let (Some(after_seq), Some(oldest_seq)) = (after_seq, oldest_seq) {
                if after_seq < oldest_seq.saturating_sub(1) {
                    tracing::debug!(
                        target: TRACE_TARGET,
                        event_name = "pocopine.events.memory.replay_gap",
                        requested_after = after_seq,
                        oldest_retained = oldest_seq,
                    );
                    return Ok(ReplayBatch::from_events(Vec::new(), true));
                }
            }

            let mut events = Vec::new();
            for event in &state.events {
                if events.len() >= request.limit {
                    break;
                }
                let seq = parse_memory_cursor(&event.cursor)?;
                if after_seq.is_some_and(|after| seq <= after) {
                    continue;
                }
                if request.topics.iter().any(|topic| topic == &event.topic) {
                    events.push(event.clone());
                }
            }

            Ok(ReplayBatch::from_events(events, false))
        }

        /// Open a replay-only subscription immediately.
        pub fn subscribe_now(&self, request: SubscribeRequest) -> EventResult<EventSubscription> {
            validate_replay_request(&request.topics, request.replay_limit)?;
            let replay = self.replay_now(ReplayRequest {
                topics: request.topics.clone(),
                after: request.after,
                limit: request.replay_limit,
            })?;

            Ok(EventSubscription {
                topics: request.topics,
                replay,
            })
        }

        /// Open a live subscription immediately.
        pub fn subscribe_live_now(
            &self,
            request: SubscribeRequest,
        ) -> EventResult<LiveEventSubscription> {
            let opening = self.subscribe_now(request)?;
            let receiver = Box::new(MemoryEventReceiver {
                topics: opening.topics.clone(),
                receiver: self.publisher.subscribe(),
            });
            Ok(LiveEventSubscription::new(opening, receiver))
        }

        fn lock_state(&self) -> EventResult<std::sync::MutexGuard<'_, MemoryEventState>> {
            self.state
                .lock()
                .map_err(|_| EventError::Backend("memory event backend lock poisoned".into()))
        }
    }

    impl Default for MemoryEventBackend {
        fn default() -> Self {
            Self::new()
        }
    }

    impl EventBackend for MemoryEventBackend {
        fn capabilities(&self) -> EventBackendCapabilities {
            EventBackendCapabilities::memory()
        }

        fn publish<'a>(&'a self, draft: EventDraft) -> EventFuture<'a, EventEnvelope> {
            Box::pin(async move { self.publish_now(draft) })
        }

        fn replay<'a>(&'a self, request: ReplayRequest) -> EventFuture<'a, ReplayBatch> {
            Box::pin(async move { self.replay_now(request) })
        }

        fn subscribe<'a>(
            &'a self,
            request: SubscribeRequest,
        ) -> EventFuture<'a, EventSubscription> {
            Box::pin(async move { self.subscribe_now(request) })
        }
    }

    impl LiveEventBackend for MemoryEventBackend {
        fn subscribe_live<'a>(
            &'a self,
            request: SubscribeRequest,
        ) -> EventFuture<'a, LiveEventSubscription> {
            Box::pin(async move { self.subscribe_live_now(request) })
        }
    }

    struct MemoryEventReceiver {
        topics: Vec<Topic>,
        receiver: broadcast::Receiver<EventEnvelope>,
    }

    impl EventReceiver for MemoryEventReceiver {
        fn next<'a>(&'a mut self) -> EventReceiverFuture<'a> {
            Box::pin(async move {
                loop {
                    match self.receiver.recv().await {
                        Ok(event) if self.topics.iter().any(|topic| topic == &event.topic) => {
                            return Ok(Some(event));
                        }
                        Ok(_) => continue,
                        Err(broadcast::error::RecvError::Closed) => return Ok(None),
                        Err(broadcast::error::RecvError::Lagged(skipped)) => {
                            return Err(EventError::SubscriptionLagged { skipped });
                        }
                    }
                }
            })
        }
    }

    fn validate_replay_request(topics: &[Topic], limit: usize) -> EventResult<()> {
        if topics.is_empty() {
            return Err(EventError::InvalidValue {
                field: "topics",
                value: "[]".to_string(),
            });
        }
        if limit == 0 {
            return Err(EventError::InvalidValue {
                field: "limit",
                value: "0".to_string(),
            });
        }
        Ok(())
    }

    fn parse_memory_cursor(cursor: &EventCursor) -> EventResult<u64> {
        let value = cursor.as_str();
        let Some(seq) = value.strip_prefix("memory:") else {
            return Err(EventError::InvalidValue {
                field: "event cursor",
                value: value.to_string(),
            });
        };
        seq.parse::<u64>().map_err(|_| EventError::InvalidValue {
            field: "event cursor",
            value: value.to_string(),
        })
    }

    fn epoch_ms() -> EventResult<u64> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|err| EventError::Time(err.to_string()))?;
        Ok(duration.as_millis().min(u128::from(u64::MAX)) as u64)
    }

    #[cfg(feature = "redis")]
    mod redis_backend {
        use std::pin::Pin;
        use std::sync::OnceLock;

        use futures_util::stream::{Stream, StreamExt};
        use pocopine_observe::{LOG_TARGET, TRACE_TARGET};
        use redis::aio::MultiplexedConnection;
        use redis::streams::{StreamId, StreamRangeReply};

        use super::{validate_replay_request, EventBackend, EventBackendCapabilities};
        use crate::{
            Audience, EventCursor, EventDraft, EventEnvelope, EventError, EventFuture, EventId,
            EventKind, EventReceiver, EventReceiverFuture, EventResult, EventSubscription,
            LiveEventBackend, LiveEventSubscription, ReplayBatch, ReplayRequest, SubscribeRequest,
            Topic,
        };

        const DEFAULT_STREAM_MAX_LEN: usize = 10_000;
        const REDIS_CURSOR_PREFIX: &str = "redis:";
        const PUBLISH_SCRIPT_SRC: &str = r#"
local now_ms = tonumber(ARGV[2])
if now_ms == 0 then
  local now = redis.call('TIME')
  now_ms = now[1] * 1000 + math.floor(now[2] / 1000)
end

local id = redis.call(
  'XADD',
  KEYS[1],
  'MAXLEN',
  '~',
  ARGV[1],
  '*',
  'protocol',
  ARGV[3],
  'topic',
  ARGV[4],
  'kind',
  ARGV[5],
  'audience',
  ARGV[6],
  'payload',
  ARGV[7],
  'created_at_ms',
  now_ms,
  'schema_version',
  ARGV[8]
)
redis.call('PUBLISH', KEYS[2], id)
return {id, tostring(now_ms)}
"#;

        fn publish_script() -> &'static redis::Script {
            static SCRIPT: OnceLock<redis::Script> = OnceLock::new();
            SCRIPT.get_or_init(|| redis::Script::new(PUBLISH_SCRIPT_SRC))
        }

        /// Redis Streams/PubSub event backend configuration.
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub struct RedisEventConfig {
            /// Redis URL. Production callers must provide this explicitly.
            pub url: String,
            /// Pocopine application namespace used in Redis keys.
            pub app: String,
            /// Approximate maximum number of retained stream entries.
            pub stream_max_len: usize,
        }

        impl RedisEventConfig {
            /// Build a Redis event backend config with default stream retention.
            pub fn new(url: impl Into<String>, app: impl Into<String>) -> EventResult<Self> {
                let url = crate::validate_non_empty("redis url", url.into())?;
                let app = crate::validate_non_empty("redis app", app.into())?;
                validate_redis_app(&app)?;
                Ok(Self {
                    url,
                    app,
                    stream_max_len: DEFAULT_STREAM_MAX_LEN,
                })
            }

            /// Set the approximate maximum retained stream length.
            pub fn with_stream_max_len(mut self, stream_max_len: usize) -> EventResult<Self> {
                if stream_max_len == 0 {
                    return Err(EventError::InvalidRetention(
                        "stream_max_len must be greater than zero".to_string(),
                    ));
                }
                self.stream_max_len = stream_max_len;
                Ok(self)
            }

            /// Redis stream key used for durable replay.
            pub fn stream_key(&self) -> String {
                format!("pocopine:{{{}}}:events:stream", self.app)
            }

            /// Redis Pub/Sub channel used for live wake-ups.
            pub fn pubsub_key(&self) -> String {
                format!("pocopine:{{{}}}:events:pubsub", self.app)
            }
        }

        /// Redis Streams backend with Pub/Sub wake-ups.
        ///
        /// Events are stored in one app-wide stream so cursors remain
        /// globally ordered across topics. The `{app}` hash tag keeps the
        /// stream and wake-up channel in the same Redis Cluster slot.
        #[derive(Clone, Debug)]
        pub struct RedisEventBackend {
            config: RedisEventConfig,
            client: redis::Client,
            connection: std::sync::Arc<std::sync::Mutex<Option<MultiplexedConnection>>>,
        }

        impl RedisEventBackend {
            /// Build a Redis backend from explicit config.
            pub fn from_config(config: RedisEventConfig) -> EventResult<Self> {
                let client = redis::Client::open(config.url.as_str())?;
                Ok(Self {
                    config,
                    client,
                    connection: std::sync::Arc::new(std::sync::Mutex::new(None)),
                })
            }

            /// Build a Redis backend from `POCOPINE_REDIS_URL`.
            ///
            /// This deliberately does not default to localhost. Apps that
            /// choose Redis should fail loudly if the broker URL is missing.
            pub fn from_env(app: impl Into<String>) -> EventResult<Self> {
                let url =
                    std::env::var("POCOPINE_REDIS_URL").map_err(|_| EventError::InvalidValue {
                        field: "POCOPINE_REDIS_URL",
                        value: "missing".to_string(),
                    })?;
                Self::from_config(RedisEventConfig::new(url, app)?)
            }

            /// Redis stream key used for durable replay.
            pub fn stream_key(&self) -> String {
                self.config.stream_key()
            }

            /// Redis Pub/Sub channel used for live wake-ups.
            pub fn pubsub_key(&self) -> String {
                self.config.pubsub_key()
            }

            /// Drop the cached multiplexed connection. The next operation
            /// opens a fresh Redis connection.
            pub fn clear_connection(&self) -> EventResult<()> {
                let mut cached = self.connection.lock().map_err(|_| {
                    EventError::Backend("cached Redis event connection mutex was poisoned".into())
                })?;
                *cached = None;
                Ok(())
            }

            async fn publish_to_stream(&self, draft: EventDraft) -> EventResult<EventEnvelope> {
                let stream_key = self.stream_key();
                let pubsub_key = self.pubsub_key();
                let audience = serde_json::to_string(&draft.audience)?;
                let payload = serde_json::to_string(&draft.payload)?;
                let mut conn = self.connection().await?;
                let (stream_id, created_at_raw): (String, String) = publish_script()
                    .key(&stream_key)
                    .key(&pubsub_key)
                    .arg(self.config.stream_max_len)
                    .arg(draft.created_at_ms)
                    .arg(&draft.protocol)
                    .arg(draft.topic.as_str())
                    .arg(draft.kind.as_str())
                    .arg(audience)
                    .arg(payload)
                    .arg(draft.schema_version)
                    .invoke_async(&mut conn)
                    .await?;
                let created_at_ms = created_at_raw.parse::<u64>().map_err(|err| {
                    EventError::Backend(format!(
                        "Redis event script returned invalid timestamp `{created_at_raw}`: {err}"
                    ))
                })?;
                let id = redis_event_id(&stream_id)?;
                let cursor = redis_event_cursor(&stream_id)?;
                let envelope = EventEnvelope::from_draft(draft, id, cursor, created_at_ms);

                tracing::debug!(
                    target: TRACE_TARGET,
                    event_name = "pocopine.events.redis.publish",
                    topic = %envelope.topic,
                    kind = %envelope.kind,
                    cursor = %envelope.cursor,
                );

                Ok(envelope)
            }

            async fn replay_from_stream(&self, request: ReplayRequest) -> EventResult<ReplayBatch> {
                validate_replay_request(&request.topics, request.limit)?;
                let stream_key = self.stream_key();
                let mut conn = self.connection().await?;
                let after_id = request
                    .after
                    .as_ref()
                    .map(redis_stream_id_from_cursor)
                    .transpose()?
                    .map(str::to_string);

                if let Some(after_id) = after_id.as_deref() {
                    if let Some(oldest_id) = oldest_stream_id(&mut conn, &stream_key).await? {
                        if redis_stream_id_before(after_id, &oldest_id)? {
                            tracing::debug!(
                                target: TRACE_TARGET,
                                event_name = "pocopine.events.redis.replay_gap",
                                requested_after = after_id,
                                oldest_retained = oldest_id,
                            );
                            return Ok(ReplayBatch::from_events(Vec::new(), true));
                        }
                    }
                }

                let mut start = after_id
                    .as_ref()
                    .map(|id| format!("({id}"))
                    .unwrap_or_else(|| "-".to_string());
                let scan_count = request.limit.saturating_mul(4).max(16);
                let mut events = Vec::new();

                while events.len() < request.limit {
                    let reply =
                        xrange_count(&mut conn, &stream_key, &start, "+", scan_count).await?;
                    let returned = reply.ids.len();
                    if returned == 0 {
                        break;
                    }

                    let mut last_scanned_id = None;
                    for entry in reply.ids {
                        last_scanned_id = Some(entry.id.clone());
                        let envelope = stream_entry_to_envelope(entry)?;
                        if request.topics.iter().any(|topic| topic == &envelope.topic) {
                            events.push(envelope);
                            if events.len() >= request.limit {
                                break;
                            }
                        }
                    }

                    let Some(last_scanned_id) = last_scanned_id else {
                        break;
                    };
                    start = format!("({last_scanned_id}");
                    if returned < scan_count {
                        break;
                    }
                }

                Ok(ReplayBatch::from_events(events, false))
            }

            async fn subscribe_to_stream(
                &self,
                request: SubscribeRequest,
            ) -> EventResult<EventSubscription> {
                validate_replay_request(&request.topics, request.replay_limit)?;
                let replay = self
                    .replay_from_stream(ReplayRequest {
                        topics: request.topics.clone(),
                        after: request.after,
                        limit: request.replay_limit,
                    })
                    .await?;
                Ok(EventSubscription {
                    topics: request.topics,
                    replay,
                })
            }

            async fn subscribe_live_to_stream(
                &self,
                request: SubscribeRequest,
            ) -> EventResult<LiveEventSubscription> {
                let resume_after = request.after.clone();
                let mut pubsub = self.client.get_async_pubsub().await?;
                pubsub.subscribe(self.pubsub_key()).await?;
                let opening = self.subscribe_to_stream(request).await?;
                let last_seen_id = opening
                    .replay
                    .cursor
                    .as_ref()
                    .or(resume_after.as_ref())
                    .map(redis_stream_id_from_cursor)
                    .transpose()?
                    .map(str::to_string);
                let receiver = RedisEventReceiver {
                    backend: self.clone(),
                    topics: opening.topics.clone(),
                    last_seen_id,
                    stream: Box::pin(pubsub.into_on_message()),
                };
                Ok(LiveEventSubscription::new(opening, Box::new(receiver)))
            }

            async fn fetch_envelope(&self, stream_id: &str) -> EventResult<Option<EventEnvelope>> {
                let stream_key = self.stream_key();
                let mut conn = self.connection().await?;
                let reply = xrange_count(&mut conn, &stream_key, stream_id, stream_id, 1).await?;
                reply
                    .ids
                    .into_iter()
                    .next()
                    .map(stream_entry_to_envelope)
                    .transpose()
            }

            async fn connection(&self) -> EventResult<MultiplexedConnection> {
                if let Some(conn) = self.cached_connection()? {
                    return Ok(conn);
                }

                let conn = self.client.get_multiplexed_async_connection().await?;
                let mut cached = self.connection.lock().map_err(|_| {
                    EventError::Backend("cached Redis event connection mutex was poisoned".into())
                })?;
                if let Some(existing) = cached.as_ref() {
                    return Ok(existing.clone());
                }
                *cached = Some(conn.clone());
                Ok(conn)
            }

            fn cached_connection(&self) -> EventResult<Option<MultiplexedConnection>> {
                Ok(self
                    .connection
                    .lock()
                    .map_err(|_| {
                        EventError::Backend(
                            "cached Redis event connection mutex was poisoned".into(),
                        )
                    })?
                    .clone())
            }
        }

        impl EventBackend for RedisEventBackend {
            fn capabilities(&self) -> EventBackendCapabilities {
                EventBackendCapabilities::redis_streams()
            }

            fn publish<'a>(&'a self, draft: EventDraft) -> EventFuture<'a, EventEnvelope> {
                Box::pin(async move { self.publish_to_stream(draft).await })
            }

            fn replay<'a>(&'a self, request: ReplayRequest) -> EventFuture<'a, ReplayBatch> {
                Box::pin(async move { self.replay_from_stream(request).await })
            }

            fn subscribe<'a>(
                &'a self,
                request: SubscribeRequest,
            ) -> EventFuture<'a, EventSubscription> {
                Box::pin(async move { self.subscribe_to_stream(request).await })
            }
        }

        impl LiveEventBackend for RedisEventBackend {
            fn subscribe_live<'a>(
                &'a self,
                request: SubscribeRequest,
            ) -> EventFuture<'a, LiveEventSubscription> {
                Box::pin(async move { self.subscribe_live_to_stream(request).await })
            }
        }

        struct RedisEventReceiver {
            backend: RedisEventBackend,
            topics: Vec<Topic>,
            last_seen_id: Option<String>,
            stream: Pin<Box<dyn Stream<Item = redis::Msg> + Send>>,
        }

        impl EventReceiver for RedisEventReceiver {
            fn next<'a>(&'a mut self) -> EventReceiverFuture<'a> {
                Box::pin(async move {
                    loop {
                        let Some(message) = self.stream.as_mut().next().await else {
                            return Ok(None);
                        };
                        let stream_id: String = match message.get_payload() {
                            Ok(stream_id) => stream_id,
                            Err(err) => {
                                tracing::warn!(
                                    target: LOG_TARGET,
                                    event_name = "pocopine.events.redis.pubsub_bad_payload",
                                    error = %err,
                                );
                                continue;
                            }
                        };

                        if self.has_seen(&stream_id)? {
                            continue;
                        }
                        self.mark_seen(&stream_id)?;

                        let Some(event) = self.backend.fetch_envelope(&stream_id).await? else {
                            tracing::debug!(
                                target: TRACE_TARGET,
                                event_name = "pocopine.events.redis.pubsub_missing_stream_entry",
                                redis_stream_id = stream_id,
                            );
                            continue;
                        };
                        if self.topics.iter().any(|topic| topic == &event.topic) {
                            return Ok(Some(event));
                        }
                    }
                })
            }
        }

        impl RedisEventReceiver {
            fn has_seen(&self, stream_id: &str) -> EventResult<bool> {
                if let Some(last_seen) = self.last_seen_id.as_deref() {
                    return Ok(!redis_stream_id_after(stream_id, last_seen)?);
                }
                Ok(false)
            }

            fn mark_seen(&mut self, stream_id: &str) -> EventResult<()> {
                if self
                    .last_seen_id
                    .as_deref()
                    .map(|last_seen| redis_stream_id_after(stream_id, last_seen))
                    .transpose()?
                    .unwrap_or(true)
                {
                    self.last_seen_id = Some(stream_id.to_string());
                }
                Ok(())
            }
        }

        async fn xrange_count(
            conn: &mut MultiplexedConnection,
            stream_key: &str,
            start: &str,
            end: &str,
            count: usize,
        ) -> EventResult<StreamRangeReply> {
            Ok(redis::cmd("XRANGE")
                .arg(stream_key)
                .arg(start)
                .arg(end)
                .arg("COUNT")
                .arg(count)
                .query_async(conn)
                .await?)
        }

        async fn oldest_stream_id(
            conn: &mut MultiplexedConnection,
            stream_key: &str,
        ) -> EventResult<Option<String>> {
            let reply = xrange_count(conn, stream_key, "-", "+", 1).await?;
            Ok(reply.ids.first().map(|entry| entry.id.clone()))
        }

        fn stream_entry_to_envelope(entry: StreamId) -> EventResult<EventEnvelope> {
            let stream_id = entry.id.clone();
            let protocol: String = required_stream_field(&entry, "protocol")?;
            let topic = Topic::new(required_stream_field::<String>(&entry, "topic")?)?;
            let kind = EventKind::new(required_stream_field::<String>(&entry, "kind")?)?;
            let audience: Audience =
                serde_json::from_str(&required_stream_field::<String>(&entry, "audience")?)?;
            let payload: serde_json::Value =
                serde_json::from_str(&required_stream_field::<String>(&entry, "payload")?)?;
            let created_at_ms: u64 = required_stream_field(&entry, "created_at_ms")?;
            let schema_version: u32 = required_stream_field(&entry, "schema_version")?;
            let draft = EventDraft {
                protocol,
                topic,
                kind,
                audience,
                payload,
                created_at_ms,
                schema_version,
            };
            Ok(EventEnvelope::from_draft(
                draft,
                redis_event_id(&stream_id)?,
                redis_event_cursor(&stream_id)?,
                created_at_ms,
            ))
        }

        fn required_stream_field<T: redis::FromRedisValue>(
            entry: &StreamId,
            field: &'static str,
        ) -> EventResult<T> {
            entry.get(field).ok_or_else(|| EventError::InvalidValue {
                field,
                value: format!("missing Redis stream field `{field}`"),
            })
        }

        fn redis_event_id(stream_id: &str) -> EventResult<EventId> {
            EventId::new(format!("{REDIS_CURSOR_PREFIX}{stream_id}"))
        }

        fn redis_event_cursor(stream_id: &str) -> EventResult<EventCursor> {
            EventCursor::new(format!("{REDIS_CURSOR_PREFIX}{stream_id}"))
        }

        fn redis_stream_id_from_cursor(cursor: &EventCursor) -> EventResult<&str> {
            let value = cursor.as_str();
            let Some(stream_id) = value.strip_prefix(REDIS_CURSOR_PREFIX) else {
                return Err(EventError::InvalidValue {
                    field: "event cursor",
                    value: value.to_string(),
                });
            };
            parse_redis_stream_id(stream_id).map(|_| stream_id)
        }

        fn redis_stream_id_before(left: &str, right: &str) -> EventResult<bool> {
            Ok(parse_redis_stream_id(left)? < parse_redis_stream_id(right)?)
        }

        fn redis_stream_id_after(left: &str, right: &str) -> EventResult<bool> {
            Ok(parse_redis_stream_id(left)? > parse_redis_stream_id(right)?)
        }

        fn parse_redis_stream_id(value: &str) -> EventResult<(u64, u64)> {
            let Some((ms, seq)) = value.split_once('-') else {
                return Err(EventError::InvalidValue {
                    field: "redis stream id",
                    value: value.to_string(),
                });
            };
            let ms = ms.parse::<u64>().map_err(|_| EventError::InvalidValue {
                field: "redis stream id",
                value: value.to_string(),
            })?;
            let seq = seq.parse::<u64>().map_err(|_| EventError::InvalidValue {
                field: "redis stream id",
                value: value.to_string(),
            })?;
            Ok((ms, seq))
        }

        fn validate_redis_app(app: &str) -> EventResult<()> {
            if app.chars().any(|ch| matches!(ch, '{' | '}')) {
                return Err(EventError::InvalidValue {
                    field: "redis app",
                    value: app.to_string(),
                });
            }
            Ok(())
        }

        #[cfg(test)]
        mod tests {
            use super::*;
            use serde_json::json;
            use std::time::{Duration, SystemTime, UNIX_EPOCH};

            #[test]
            fn rejects_bad_redis_config() {
                assert!(RedisEventConfig::new("", "app").is_err());
                assert!(RedisEventConfig::new("redis://127.0.0.1/", "bad{app").is_err());
                assert!(RedisEventConfig::new("redis://127.0.0.1/", "app")
                    .unwrap()
                    .with_stream_max_len(0)
                    .is_err());
            }

            #[test]
            fn parses_redis_cursors_strictly() {
                let cursor = EventCursor::new("redis:1700000000000-3").unwrap();
                assert_eq!(
                    redis_stream_id_from_cursor(&cursor).unwrap(),
                    "1700000000000-3"
                );
                assert!(redis_stream_id_before("1700000000000-3", "1700000000001-0").unwrap());
                assert!(
                    redis_stream_id_from_cursor(&EventCursor::new("memory:1").unwrap()).is_err()
                );
                assert!(parse_redis_stream_id("1700000000000").is_err());
            }

            #[test]
            fn redis_backend_advertises_supported_capabilities() {
                let config = RedisEventConfig::new("redis://127.0.0.1/", "events-test").unwrap();
                let backend = RedisEventBackend::from_config(config).unwrap();

                assert_eq!(
                    backend.capabilities(),
                    EventBackendCapabilities::redis_streams()
                );
            }

            #[tokio::test]
            async fn redis_publish_replay_and_live_round_trip() -> EventResult<()> {
                let Some(url) = std::env::var("REDIS_TEST_URL").ok() else {
                    return Ok(());
                };
                let app = format!(
                    "events-test-{}-{}",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|err| EventError::Time(err.to_string()))?
                        .as_millis()
                );
                let config = RedisEventConfig::new(url, app)?.with_stream_max_len(128)?;
                let backend = RedisEventBackend::from_config(config)?;
                delete_stream(&backend).await?;

                let posts = Topic::new("posts")?;
                let mut subscription = backend
                    .subscribe_live(SubscribeRequest::new([posts.clone()]))
                    .await?;

                let published = backend
                    .publish(EventDraft::new(
                        posts.clone(),
                        "collection.changed",
                        json!({ "n": 1 }),
                    )?)
                    .await?;
                let received = tokio::time::timeout(Duration::from_secs(2), subscription.next())
                    .await
                    .map_err(|err| {
                        EventError::Backend(format!("timed out waiting for Redis event: {err}"))
                    })??
                    .ok_or_else(|| EventError::Backend("Redis live stream closed".into()))?;
                let replay = backend
                    .replay(ReplayRequest::new([posts]).limit(10))
                    .await?;

                assert_eq!(received, published);
                assert_eq!(replay.events, vec![published]);

                delete_stream(&backend).await?;
                Ok(())
            }

            async fn delete_stream(backend: &RedisEventBackend) -> EventResult<()> {
                let mut conn = backend.connection().await?;
                let _: () = redis::cmd("DEL")
                    .arg(backend.stream_key())
                    .query_async(&mut conn)
                    .await?;
                Ok(())
            }
        }
    }

    #[cfg(feature = "redis")]
    pub use redis_backend::{RedisEventBackend, RedisEventConfig};

    #[cfg(test)]
    mod tests {
        use serde_json::json;

        use super::*;
        use crate::{Audience, EventKind};

        fn draft(topic: &str, n: u64) -> EventDraft {
            EventDraft::new(topic, "collection.changed", json!({ "n": n }))
                .unwrap()
                .created_at_ms(n)
        }

        #[test]
        fn rejects_empty_topics_and_cursors() {
            assert!(Topic::new("").is_err());
            assert!(Topic::new(" posts").is_err());
            assert!(EventCursor::new("\n").is_err());
            assert!(EventKind::new("collection.changed").is_ok());
        }

        #[test]
        fn rejects_zero_memory_capacity() {
            assert!(MemoryEventBackend::with_capacity(0).is_err());
        }

        #[test]
        fn memory_backend_advertises_process_local_capabilities() {
            let backend = MemoryEventBackend::new();

            assert_eq!(backend.capabilities(), EventBackendCapabilities::memory());
            assert!(!backend.capabilities().durable_replay);
            assert!(!backend.capabilities().multi_process);
        }

        #[test]
        fn publish_assigns_id_cursor_and_replays_by_topic() {
            let backend = MemoryEventBackend::with_capacity(8).unwrap();
            let posts = Topic::new("posts").unwrap();

            let first = backend.publish_now(draft("posts", 1)).unwrap();
            let second = backend.publish_now(draft("comments", 2)).unwrap();

            assert_eq!(first.id.as_str(), "memory:1");
            assert_eq!(first.cursor.as_str(), "memory:1");
            assert_eq!(second.cursor.as_str(), "memory:2");

            let replay = backend
                .replay_now(ReplayRequest::new([posts.clone()]).limit(10))
                .unwrap();

            assert!(!replay.gap);
            assert_eq!(replay.events.len(), 1);
            assert_eq!(replay.events[0].topic, posts);
            assert_eq!(
                replay.cursor.as_ref().map(EventCursor::as_str),
                Some("memory:1")
            );
        }

        #[test]
        fn replay_after_cursor_is_strict_and_limited() {
            let backend = MemoryEventBackend::with_capacity(8).unwrap();
            let posts = Topic::new("posts").unwrap();

            let first = backend.publish_now(draft("posts", 1)).unwrap();
            backend.publish_now(draft("posts", 2)).unwrap();
            backend.publish_now(draft("posts", 3)).unwrap();

            let replay = backend
                .replay_now(
                    ReplayRequest::new([posts])
                        .after(Some(first.cursor))
                        .limit(1),
                )
                .unwrap();

            assert!(!replay.gap);
            assert_eq!(replay.events.len(), 1);
            assert_eq!(replay.events[0].payload, json!({ "n": 2 }));
            assert_eq!(
                replay.cursor.as_ref().map(EventCursor::as_str),
                Some("memory:2")
            );
        }

        #[test]
        fn replay_reports_gap_when_cursor_is_evicted() {
            let backend = MemoryEventBackend::with_capacity(2).unwrap();
            let posts = Topic::new("posts").unwrap();

            let first = backend.publish_now(draft("posts", 1)).unwrap();
            backend.publish_now(draft("posts", 2)).unwrap();
            backend.publish_now(draft("posts", 3)).unwrap();
            backend.publish_now(draft("posts", 4)).unwrap();

            let replay = backend
                .replay_now(ReplayRequest::new([posts]).after(Some(first.cursor)))
                .unwrap();

            assert!(replay.gap);
            assert!(replay.events.is_empty());
            assert!(replay.cursor.is_none());
        }

        #[test]
        fn subscribe_returns_opening_replay() {
            let backend = MemoryEventBackend::with_capacity(4).unwrap();
            let posts = Topic::new("posts").unwrap();
            let first = backend
                .publish_now(
                    draft("posts", 1)
                        .audience(Audience::Public)
                        .schema_version(2),
                )
                .unwrap();

            let subscription = backend
                .subscribe_now(SubscribeRequest::new([posts.clone()]).after(None))
                .unwrap();

            assert_eq!(subscription.topics, vec![posts]);
            assert_eq!(subscription.replay.events, vec![first]);
        }

        #[test]
        fn replay_rejects_bad_cursor_and_empty_topics() {
            let backend = MemoryEventBackend::new();
            let bad_cursor = EventCursor::new("redis:1-0").unwrap();
            let posts = Topic::new("posts").unwrap();

            assert!(backend
                .replay_now(ReplayRequest::new([posts]).after(Some(bad_cursor)))
                .is_err());
            assert!(backend.replay_now(ReplayRequest::new([])).is_err());
        }

        #[tokio::test]
        async fn live_subscription_receives_matching_future_events() {
            let backend = MemoryEventBackend::with_capacity(8).unwrap();
            let posts = Topic::new("posts").unwrap();
            let mut subscription = backend
                .subscribe_live_now(SubscribeRequest::new([posts.clone()]))
                .unwrap();

            backend.publish_now(draft("comments", 1)).unwrap();
            let published = backend.publish_now(draft("posts", 2)).unwrap();

            let received = subscription.next().await.unwrap().unwrap();

            assert_eq!(subscription.opening.topics, vec![posts]);
            assert_eq!(received, published);
        }

        #[tokio::test]
        async fn live_subscription_reports_lag() {
            let backend = MemoryEventBackend::with_config(MemoryEventConfig {
                capacity: 8,
                subscriber_capacity: 1,
            })
            .unwrap();
            let posts = Topic::new("posts").unwrap();
            let mut subscription = backend
                .subscribe_live_now(SubscribeRequest::new([posts]))
                .unwrap();

            backend.publish_now(draft("posts", 1)).unwrap();
            backend.publish_now(draft("posts", 2)).unwrap();
            backend.publish_now(draft("posts", 3)).unwrap();

            let err = subscription.next().await.unwrap_err();

            assert!(matches!(err, EventError::SubscriptionLagged { skipped: _ }));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use host::{MemoryEventBackend, MemoryEventConfig};
#[cfg(all(feature = "redis", not(target_arch = "wasm32")))]
pub use host::{RedisEventBackend, RedisEventConfig};
