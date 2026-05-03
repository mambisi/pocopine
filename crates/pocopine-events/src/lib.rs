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
    /// A backend lock was poisoned.
    Backend(String),
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
            Self::Backend(msg) => write!(f, "event backend error: {msg}"),
            Self::Time(msg) => write!(f, "time error: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
        }
    }
}

impl std::error::Error for EventError {}

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
        EventBackend, EventCursor, EventDraft, EventEnvelope, EventError, EventFuture, EventId,
        EventReceiver, EventReceiverFuture, EventResult, EventSubscription, LiveEventBackend,
        LiveEventSubscription, ReplayBatch, ReplayRequest, SubscribeRequest, Topic,
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
