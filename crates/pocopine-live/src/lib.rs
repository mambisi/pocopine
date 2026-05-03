//! Browser-facing live invalidation streams for pocopine apps.
//!
//! `pocopine-live` builds on `pocopine-events`: the events crate owns the
//! neutral event spine, while this crate owns the safe browser protocol
//! for collection/query invalidation.

use std::fmt;

use pocopine_events::{EventDraft, EventError, EventKind, EventResult, IntoTopic, Topic};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Current browser live protocol identifier.
pub const LIVE_PROTOCOL_V1: &str = "pocopine.live.v1";

/// SSE endpoint mounted by [`routes`].
pub const LIVE_STREAM_PATH: &str = "/__pocopine/live/v1/stream";

/// Event emitted when a live stream is accepted.
pub const KIND_READY: &str = "ready";
/// Event emitted when a collection may have changed.
pub const KIND_COLLECTION_CHANGED: &str = "collection.changed";
/// Event emitted when a collection key was deleted.
pub const KIND_COLLECTION_DELETED: &str = "collection.deleted";
/// Event emitted when one or more query tags should refetch.
pub const KIND_QUERY_INVALIDATED: &str = "query.invalidated";
/// Event emitted when a replay cursor is too old.
pub const KIND_GAP: &str = "gap";
/// Event emitted when the live stream has a typed failure.
pub const KIND_ERROR: &str = "error";

#[cfg(target_arch = "wasm32")]
const BUILT_IN_EVENT_KINDS: &[&str] = &[
    KIND_READY,
    KIND_COLLECTION_CHANGED,
    KIND_COLLECTION_DELETED,
    KIND_QUERY_INVALIDATED,
    KIND_GAP,
    KIND_ERROR,
];

/// Operation attached to a collection invalidation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveOp {
    /// One or more records may have been inserted or updated.
    Upsert,
    /// One or more records may have been deleted.
    Delete,
    /// Client should refetch the collection/query from scratch.
    Reset,
}

/// Collection/query invalidation payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LiveInvalidation {
    /// Browser protocol identifier.
    pub protocol: String,
    /// Public framework collection name.
    pub collection: String,
    /// Change operation.
    pub op: LiveOp,
    /// Public keys affected by this invalidation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    /// Query tags affected by this invalidation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub query_tags: Vec<String>,
    /// Topic schema version.
    pub schema_version: u32,
}

impl LiveInvalidation {
    /// Build an upsert invalidation.
    pub fn upsert(collection: impl Into<String>) -> Self {
        Self::new(collection, LiveOp::Upsert)
    }

    /// Build a delete invalidation.
    pub fn delete(collection: impl Into<String>) -> Self {
        Self::new(collection, LiveOp::Delete)
    }

    /// Build a reset invalidation.
    pub fn reset(collection: impl Into<String>) -> Self {
        Self::new(collection, LiveOp::Reset)
    }

    /// Build an invalidation with an explicit operation.
    pub fn new(collection: impl Into<String>, op: LiveOp) -> Self {
        Self {
            protocol: LIVE_PROTOCOL_V1.to_string(),
            collection: collection.into(),
            op,
            keys: Vec::new(),
            query_tags: Vec::new(),
            schema_version: 1,
        }
    }

    /// Attach affected keys.
    pub fn keys(mut self, keys: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.keys = keys.into_iter().map(Into::into).collect();
        self
    }

    /// Attach affected query tags.
    pub fn query_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.query_tags = tags.into_iter().map(Into::into).collect();
        self
    }

    /// Set the schema version.
    pub fn schema_version(mut self, schema_version: u32) -> Self {
        self.schema_version = schema_version;
        self
    }

    /// Convert the invalidation into an event draft.
    pub fn into_draft(self) -> EventResult<EventDraft> {
        let topic = collection_topic(&self.collection)?;
        let kind = match self.op {
            LiveOp::Delete => EventKind::new(KIND_COLLECTION_DELETED)?,
            _ => EventKind::new(KIND_COLLECTION_CHANGED)?,
        };
        let schema_version = self.schema_version;
        let payload =
            serde_json::to_value(self).map_err(|err| EventError::Backend(err.to_string()))?;
        EventDraft::new(topic, kind, payload).map(|draft| draft.schema_version(schema_version))
    }
}

/// Build the framework topic for a collection.
pub fn collection_topic(collection: &str) -> EventResult<Topic> {
    Topic::new(format!("collection:{collection}"))
}

/// Build the framework topic for a query tag.
pub fn query_tag_topic(tag: &str) -> EventResult<Topic> {
    Topic::new(format!("query:{tag}"))
}

/// Build a query invalidation draft.
pub fn query_invalidated(
    topic: impl IntoTopic,
    tags: impl IntoIterator<Item = impl Into<String>>,
) -> EventResult<EventDraft> {
    let payload = json!({
        "protocol": LIVE_PROTOCOL_V1,
        "query_tags": tags.into_iter().map(Into::into).collect::<Vec<String>>(),
    });
    EventDraft::new(topic, KIND_QUERY_INVALIDATED, payload)
}

/// Event delivered by the browser live stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveEvent {
    /// Stream accepted and replay has started.
    Ready {
        /// Last replay cursor included in the opening frame, if any.
        cursor: Option<String>,
        /// Topics accepted by the server.
        topics: Vec<String>,
    },
    /// A collection may have changed and should usually be refetched.
    CollectionChanged {
        /// Public collection name.
        collection: String,
        /// Change operation.
        op: LiveOp,
        /// Public keys affected by this invalidation.
        keys: Vec<String>,
        /// Query tags affected by this invalidation.
        query_tags: Vec<String>,
        /// Topic schema version.
        schema_version: u32,
        /// Opaque replay cursor for this event.
        cursor: Option<String>,
        /// Backend event timestamp, milliseconds since Unix epoch.
        created_at_ms: Option<u64>,
    },
    /// One or more records were deleted.
    CollectionDeleted {
        /// Public collection name.
        collection: String,
        /// Change operation. Kept so handlers can share code with
        /// [`LiveEvent::CollectionChanged`].
        op: LiveOp,
        /// Public keys affected by this invalidation.
        keys: Vec<String>,
        /// Query tags affected by this invalidation.
        query_tags: Vec<String>,
        /// Topic schema version.
        schema_version: u32,
        /// Opaque replay cursor for this event.
        cursor: Option<String>,
        /// Backend event timestamp, milliseconds since Unix epoch.
        created_at_ms: Option<u64>,
    },
    /// One or more query tags should refetch.
    QueryInvalidated {
        /// Query tags affected by this invalidation.
        query_tags: Vec<String>,
        /// Opaque replay cursor for this event.
        cursor: Option<String>,
        /// Backend event timestamp, milliseconds since Unix epoch.
        created_at_ms: Option<u64>,
    },
    /// The requested resume cursor was no longer replayable.
    Gap {
        /// Stable reason label.
        reason: String,
    },
    /// Stream-level or client-side failure.
    Error {
        /// Stable reason label.
        reason: String,
    },
    /// Application event kind the client opted into.
    Custom {
        /// SSE event name.
        kind: String,
        /// JSON payload as delivered by the server.
        payload: Value,
        /// Opaque replay cursor for this event when present.
        cursor: Option<String>,
        /// Backend event timestamp, milliseconds since Unix epoch.
        created_at_ms: Option<u64>,
    },
}

/// Browser-live client failures.
#[derive(Debug)]
pub enum LiveClientError {
    /// The endpoint was empty.
    EmptyEndpoint,
    /// The client did not request any topic or collection.
    NoTopics,
    /// A collection/topic/cursor value is invalid.
    InvalidValue {
        /// Field name.
        field: &'static str,
        /// Rejected value.
        value: String,
    },
    /// Event-spine validation failed.
    Event(EventError),
    /// JSON could not be parsed.
    Json(serde_json::Error),
    /// Browser API failure.
    Browser(String),
}

impl fmt::Display for LiveClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyEndpoint => f.write_str("live stream endpoint is empty"),
            Self::NoTopics => f.write_str("live stream requested no topics"),
            Self::InvalidValue { field, value } => {
                write!(f, "invalid live {field}: {value:?}")
            }
            Self::Event(err) => write!(f, "event error: {err}"),
            Self::Json(err) => write!(f, "json error: {err}"),
            Self::Browser(err) => write!(f, "browser live stream error: {err}"),
        }
    }
}

impl std::error::Error for LiveClientError {}

impl From<EventError> for LiveClientError {
    fn from(err: EventError) -> Self {
        Self::Event(err)
    }
}

impl From<serde_json::Error> for LiveClientError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// Parse one SSE frame into the typed browser protocol.
pub fn parse_live_event(kind: &str, data: &str) -> Result<LiveEvent, LiveClientError> {
    match kind {
        KIND_READY => {
            let frame: ReadyFrame = serde_json::from_str(data)?;
            validate_protocol(&frame.protocol)?;
            Ok(LiveEvent::Ready {
                cursor: frame.cursor,
                topics: frame.topics,
            })
        }
        KIND_COLLECTION_CHANGED => {
            let frame: InvalidationFrame = serde_json::from_str(data)?;
            validate_protocol(&frame.protocol)?;
            Ok(LiveEvent::CollectionChanged {
                collection: frame.collection,
                op: frame.op,
                keys: frame.keys,
                query_tags: frame.query_tags,
                schema_version: frame.schema_version,
                cursor: frame.cursor,
                created_at_ms: frame.created_at_ms,
            })
        }
        KIND_COLLECTION_DELETED => {
            let frame: InvalidationFrame = serde_json::from_str(data)?;
            validate_protocol(&frame.protocol)?;
            Ok(LiveEvent::CollectionDeleted {
                collection: frame.collection,
                op: frame.op,
                keys: frame.keys,
                query_tags: frame.query_tags,
                schema_version: frame.schema_version,
                cursor: frame.cursor,
                created_at_ms: frame.created_at_ms,
            })
        }
        KIND_QUERY_INVALIDATED => {
            let frame: QueryInvalidatedFrame = serde_json::from_str(data)?;
            validate_protocol(&frame.protocol)?;
            Ok(LiveEvent::QueryInvalidated {
                query_tags: frame.query_tags,
                cursor: frame.cursor,
                created_at_ms: frame.created_at_ms,
            })
        }
        KIND_GAP => {
            let frame: ReasonFrame = serde_json::from_str(data)?;
            validate_protocol(&frame.protocol)?;
            Ok(LiveEvent::Gap {
                reason: frame.reason,
            })
        }
        KIND_ERROR => {
            let frame: ReasonFrame = serde_json::from_str(data)?;
            validate_protocol(&frame.protocol)?;
            Ok(LiveEvent::Error {
                reason: frame.reason,
            })
        }
        other => {
            let payload: Value = serde_json::from_str(data)?;
            let (cursor, created_at_ms) = stream_metadata(&payload);
            Ok(LiveEvent::Custom {
                kind: other.to_string(),
                payload,
                cursor,
                created_at_ms,
            })
        }
    }
}

/// Build the SSE stream URL for a browser client.
pub fn build_live_stream_url(
    endpoint: &str,
    collections: &[String],
    topics: &[String],
    last_event_id: Option<&str>,
) -> Result<String, LiveClientError> {
    let endpoint = endpoint.trim();
    if endpoint.is_empty() {
        return Err(LiveClientError::EmptyEndpoint);
    }
    if collections.is_empty() && topics.is_empty() {
        return Err(LiveClientError::NoTopics);
    }

    let mut params = Vec::new();
    if !collections.is_empty() {
        let values = collections
            .iter()
            .map(|collection| validate_collection(collection))
            .collect::<Result<Vec<_>, _>>()?;
        params.push(("collection", values.join(",")));
    }
    if !topics.is_empty() {
        let values = topics
            .iter()
            .map(|topic| validate_topic(topic))
            .collect::<Result<Vec<_>, _>>()?;
        params.push(("topic", values.join(",")));
    }
    if let Some(cursor) = last_event_id {
        params.push((
            "last_event_id",
            validate_csv_value("last_event_id", cursor)?,
        ));
    }

    let sep = if endpoint.contains('?') {
        if endpoint.ends_with('?') || endpoint.ends_with('&') {
            ""
        } else {
            "&"
        }
    } else {
        "?"
    };
    let query = params
        .into_iter()
        .map(|(key, value)| format!("{key}={}", encode_query_component(&value)))
        .collect::<Vec<_>>()
        .join("&");
    Ok(format!("{endpoint}{sep}{query}"))
}

#[derive(Deserialize)]
struct ReadyFrame {
    protocol: String,
    cursor: Option<String>,
    #[serde(default)]
    topics: Vec<String>,
}

#[derive(Deserialize)]
struct InvalidationFrame {
    protocol: String,
    collection: String,
    op: LiveOp,
    #[serde(default)]
    keys: Vec<String>,
    #[serde(default)]
    query_tags: Vec<String>,
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    created_at_ms: Option<u64>,
}

#[derive(Deserialize)]
struct QueryInvalidatedFrame {
    protocol: String,
    #[serde(default)]
    query_tags: Vec<String>,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    created_at_ms: Option<u64>,
}

#[derive(Deserialize)]
struct ReasonFrame {
    protocol: String,
    reason: String,
}

fn default_schema_version() -> u32 {
    1
}

fn validate_protocol(protocol: &str) -> Result<(), LiveClientError> {
    if protocol == LIVE_PROTOCOL_V1 {
        return Ok(());
    }
    Err(LiveClientError::InvalidValue {
        field: "protocol",
        value: protocol.to_string(),
    })
}

fn stream_metadata(payload: &Value) -> (Option<String>, Option<u64>) {
    let Some(map) = payload.as_object() else {
        return (None, None);
    };
    let cursor = map
        .get("cursor")
        .and_then(Value::as_str)
        .map(str::to_string);
    let created_at_ms = map.get("created_at_ms").and_then(Value::as_u64);
    (cursor, created_at_ms)
}

fn validate_collection(collection: &str) -> Result<String, LiveClientError> {
    let value = validate_csv_value("collection", collection)?;
    collection_topic(&value).map_err(LiveClientError::Event)?;
    Ok(value)
}

fn validate_topic(topic: &str) -> Result<String, LiveClientError> {
    let value = validate_csv_value("topic", topic)?;
    Topic::new(value.clone()).map_err(LiveClientError::Event)?;
    Ok(value)
}

fn validate_csv_value(field: &'static str, value: &str) -> Result<String, LiveClientError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed != value || value.contains(',') {
        return Err(LiveClientError::InvalidValue {
            field,
            value: value.to_string(),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(LiveClientError::InvalidValue {
            field,
            value: value.to_string(),
        });
    }
    Ok(value.to_string())
}

fn encode_query_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                out.push('%');
                out.push(hex((byte >> 4) & 0x0f));
                out.push(hex(byte & 0x0f));
            }
        }
    }
    out
}

fn hex(nibble: u8) -> char {
    match nibble {
        0..=9 => (b'0' + nibble) as char,
        10..=15 => (b'A' + (nibble - 10)) as char,
        _ => unreachable!(),
    }
}

#[cfg(target_arch = "wasm32")]
mod browser {
    use std::cell::RefCell;
    use std::rc::Rc;

    use pocopine_core::{current_scope_id, on_scope_unmount_for};
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use web_sys::{Event, EventSource, EventSourceInit, EventTarget, MessageEvent};

    use crate::{
        build_live_stream_url, parse_live_event, LiveClientError, LiveEvent, BUILT_IN_EVENT_KINDS,
        KIND_ERROR, LIVE_STREAM_PATH,
    };

    type Handler = Rc<RefCell<Box<dyn FnMut(LiveEvent)>>>;
    type Listener = (String, Closure<dyn FnMut(Event)>);

    /// Browser `EventSource` client for pocopine live streams.
    #[derive(Clone, Debug)]
    pub struct LiveClient {
        endpoint: String,
        collections: Vec<String>,
        topics: Vec<String>,
        event_kinds: Vec<String>,
        last_event_id: Option<String>,
        with_credentials: bool,
    }

    impl Default for LiveClient {
        fn default() -> Self {
            Self::new()
        }
    }

    impl LiveClient {
        /// Build a client for the default pocopine live endpoint.
        pub fn new() -> Self {
            Self {
                endpoint: LIVE_STREAM_PATH.to_string(),
                collections: Vec::new(),
                topics: Vec::new(),
                event_kinds: Vec::new(),
                last_event_id: None,
                with_credentials: false,
            }
        }

        /// Build a client whose stream is closed automatically when
        /// the current component scope unmounts.
        pub fn scoped() -> ScopedLiveClient {
            ScopedLiveClient::new()
        }

        /// Override the stream endpoint.
        pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
            self.endpoint = endpoint.into();
            self
        }

        /// Subscribe to a collection invalidation topic.
        pub fn collection(mut self, collection: impl Into<String>) -> Self {
            self.collections.push(collection.into());
            self
        }

        /// Subscribe to an explicit framework topic.
        pub fn topic(mut self, topic: impl Into<String>) -> Self {
            self.topics.push(topic.into());
            self
        }

        /// Subscribe to the framework topic for a query tag.
        pub fn query_tag(mut self, tag: impl Into<String>) -> Self {
            self.topics.push(format!("query:{}", tag.into()));
            self
        }

        /// Resume from an opaque stream cursor.
        pub fn last_event_id(mut self, cursor: impl Into<String>) -> Self {
            self.last_event_id = Some(cursor.into());
            self
        }

        /// Set the browser `EventSource.withCredentials` flag.
        pub fn with_credentials(mut self, enabled: bool) -> Self {
            self.with_credentials = enabled;
            self
        }

        /// Listen for an additional application event kind.
        pub fn event_kind(mut self, kind: impl Into<String>) -> Self {
            self.event_kinds.push(kind.into());
            self
        }

        /// Attach the event handler and return an opener.
        pub fn on_event<F>(self, handler: F) -> LiveClientWithHandler<F>
        where
            F: FnMut(LiveEvent) + 'static,
        {
            LiveClientWithHandler {
                client: self,
                handler,
            }
        }

        /// Open the browser stream with a handler.
        pub fn connect<F>(self, handler: F) -> Result<LiveSubscription, LiveClientError>
        where
            F: FnMut(LiveEvent) + 'static,
        {
            let url = build_live_stream_url(
                &self.endpoint,
                &self.collections,
                &self.topics,
                self.last_event_id.as_deref(),
            )?;
            let init = EventSourceInit::new();
            init.set_with_credentials(self.with_credentials);
            let source = EventSource::new_with_event_source_init_dict(&url, &init)
                .map_err(|err| LiveClientError::Browser(format!("{err:?}")))?;
            let handler: Handler = Rc::new(RefCell::new(Box::new(handler)));
            let mut kinds = BUILT_IN_EVENT_KINDS
                .iter()
                .map(|kind| (*kind).to_string())
                .chain(self.event_kinds)
                .collect::<Vec<_>>();
            kinds.sort();
            kinds.dedup();

            let target: EventTarget = source.clone().into();
            let mut listeners = Vec::with_capacity(kinds.len());
            for kind in kinds {
                let handler = handler.clone();
                let kind_for_parse = kind.clone();
                let closure = Closure::wrap(Box::new(move |event: Event| {
                    let parsed = live_event_from_dom_event(&kind_for_parse, event);
                    (handler.borrow_mut())(parsed);
                }) as Box<dyn FnMut(Event)>);
                target
                    .add_event_listener_with_callback(&kind, closure.as_ref().unchecked_ref())
                    .map_err(|err| LiveClientError::Browser(format!("{err:?}")))?;
                listeners.push((kind, closure));
            }

            Ok(LiveSubscription {
                source,
                listeners,
                active: true,
            })
        }

        /// Open the stream and bind its lifetime to the current
        /// component scope. Returns an error outside a handler /
        /// lifecycle context.
        pub fn connect_scoped<F>(self, handler: F) -> Result<(), LiveClientError>
        where
            F: FnMut(LiveEvent) + 'static,
        {
            let scope_id = current_scope_id().ok_or_else(|| {
                LiveClientError::Browser(
                    "LiveClient::scoped used outside a component handler or lifecycle hook"
                        .to_string(),
                )
            })?;
            let subscription = self.connect(handler)?;
            on_scope_unmount_for(scope_id, move || drop(subscription));
            Ok(())
        }
    }

    /// Scope-bound browser `EventSource` client builder.
    #[derive(Clone, Debug)]
    pub struct ScopedLiveClient {
        client: LiveClient,
    }

    impl Default for ScopedLiveClient {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ScopedLiveClient {
        /// Build a scope-bound client for the default pocopine live
        /// endpoint.
        pub fn new() -> Self {
            Self {
                client: LiveClient::new(),
            }
        }

        /// Override the stream endpoint.
        pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
            self.client = self.client.endpoint(endpoint);
            self
        }

        /// Subscribe to a collection invalidation topic.
        pub fn collection(mut self, collection: impl Into<String>) -> Self {
            self.client = self.client.collection(collection);
            self
        }

        /// Subscribe to an explicit framework topic.
        pub fn topic(mut self, topic: impl Into<String>) -> Self {
            self.client = self.client.topic(topic);
            self
        }

        /// Subscribe to the framework topic for a query tag.
        pub fn query_tag(mut self, tag: impl Into<String>) -> Self {
            self.client = self.client.query_tag(tag);
            self
        }

        /// Resume from an opaque stream cursor.
        pub fn last_event_id(mut self, cursor: impl Into<String>) -> Self {
            self.client = self.client.last_event_id(cursor);
            self
        }

        /// Set the browser `EventSource.withCredentials` flag.
        pub fn with_credentials(mut self, enabled: bool) -> Self {
            self.client = self.client.with_credentials(enabled);
            self
        }

        /// Listen for an additional application event kind.
        pub fn event_kind(mut self, kind: impl Into<String>) -> Self {
            self.client = self.client.event_kind(kind);
            self
        }

        /// Attach the event handler and return an opener.
        pub fn on_event<F>(self, handler: F) -> ScopedLiveClientWithHandler<F>
        where
            F: FnMut(LiveEvent) + 'static,
        {
            ScopedLiveClientWithHandler {
                client: self,
                handler,
            }
        }

        /// Open the stream and close it automatically on scope unmount.
        pub fn connect<F>(self, handler: F) -> Result<(), LiveClientError>
        where
            F: FnMut(LiveEvent) + 'static,
        {
            self.client.connect_scoped(handler)
        }
    }

    /// Builder returned by [`LiveClient::on_event`].
    pub struct LiveClientWithHandler<F>
    where
        F: FnMut(LiveEvent) + 'static,
    {
        client: LiveClient,
        handler: F,
    }

    impl<F> LiveClientWithHandler<F>
    where
        F: FnMut(LiveEvent) + 'static,
    {
        /// Open the stream.
        pub fn open(self) -> Result<LiveSubscription, LiveClientError> {
            self.client.connect(self.handler)
        }

        /// Open the stream and bind it to the current component
        /// scope.
        pub fn open_scoped(self) -> Result<(), LiveClientError> {
            self.client.connect_scoped(self.handler)
        }
    }

    /// Builder returned by [`ScopedLiveClient::on_event`].
    pub struct ScopedLiveClientWithHandler<F>
    where
        F: FnMut(LiveEvent) + 'static,
    {
        client: ScopedLiveClient,
        handler: F,
    }

    impl<F> ScopedLiveClientWithHandler<F>
    where
        F: FnMut(LiveEvent) + 'static,
    {
        /// Open the stream and close it automatically on scope unmount.
        pub fn open(self) -> Result<(), LiveClientError> {
            self.client.connect(self.handler)
        }
    }

    /// Active browser live subscription. Dropping it closes the stream.
    #[must_use = "dropping LiveSubscription closes the EventSource stream immediately"]
    pub struct LiveSubscription {
        source: EventSource,
        listeners: Vec<Listener>,
        active: bool,
    }

    impl LiveSubscription {
        /// Close the stream now.
        pub fn close(mut self) {
            self.close_in_place();
        }

        fn close_in_place(&mut self) {
            if !self.active {
                return;
            }
            for (kind, closure) in self.listeners.drain(..) {
                let _ = self
                    .source
                    .remove_event_listener_with_callback(&kind, closure.as_ref().unchecked_ref());
            }
            self.source.close();
            self.active = false;
        }
    }

    impl Drop for LiveSubscription {
        fn drop(&mut self) {
            self.close_in_place();
        }
    }

    fn live_event_from_dom_event(kind: &str, event: Event) -> LiveEvent {
        let Ok(message) = event.dyn_into::<MessageEvent>() else {
            return LiveEvent::Error {
                reason: if kind == KIND_ERROR {
                    "eventsource_error".to_string()
                } else {
                    "eventsource_non_message_event".to_string()
                },
            };
        };
        let Some(data) = message.data().as_string() else {
            return LiveEvent::Error {
                reason: "eventsource_non_string_data".to_string(),
            };
        };
        parse_live_event(kind, &data).unwrap_or_else(|err| LiveEvent::Error {
            reason: err.to_string(),
        })
    }
}

/// Live stream failures.
#[derive(Debug)]
pub enum LiveError {
    /// Event-spine failure.
    Event(EventError),
    /// JSON serialization failure.
    Json(serde_json::Error),
    /// URL query string could not be decoded.
    BadQuery(String),
    /// Request asked for no topics.
    NoTopics,
    /// Request asked for a topic rejected by policy.
    ForbiddenTopic(String),
}

impl fmt::Display for LiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Event(err) => write!(f, "event error: {err}"),
            Self::Json(err) => write!(f, "json error: {err}"),
            Self::BadQuery(err) => write!(f, "bad live query: {err}"),
            Self::NoTopics => f.write_str("live stream requested no topics"),
            Self::ForbiddenTopic(topic) => write!(f, "live topic is forbidden: {topic}"),
        }
    }
}

impl std::error::Error for LiveError {}

impl From<EventError> for LiveError {
    fn from(err: EventError) -> Self {
        Self::Event(err)
    }
}

impl From<serde_json::Error> for LiveError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn with_stream_metadata(
    mut payload: serde_json::Value,
    cursor: &str,
    created_at_ms: u64,
) -> serde_json::Value {
    match &mut payload {
        serde_json::Value::Object(map) => {
            map.entry("protocol")
                .or_insert_with(|| serde_json::Value::String(LIVE_PROTOCOL_V1.to_string()));
            map.insert(
                "cursor".to_string(),
                serde_json::Value::String(cursor.to_string()),
            );
            map.insert(
                "created_at_ms".to_string(),
                serde_json::Value::Number(created_at_ms.into()),
            );
            payload
        }
        _ => json!({
            "protocol": LIVE_PROTOCOL_V1,
            "cursor": cursor,
            "created_at_ms": created_at_ms,
            "payload": payload,
        }),
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;

    #[test]
    fn parses_ready_frame() {
        let event = parse_live_event(
            KIND_READY,
            r#"{"protocol":"pocopine.live.v1","cursor":"memory:1","topics":["collection:posts"]}"#,
        )
        .unwrap();

        assert_eq!(
            event,
            LiveEvent::Ready {
                cursor: Some("memory:1".to_string()),
                topics: vec!["collection:posts".to_string()],
            }
        );
    }

    #[test]
    fn parses_collection_changed_frame() {
        let event = parse_live_event(
            KIND_COLLECTION_CHANGED,
            r#"{
                "protocol":"pocopine.live.v1",
                "collection":"posts",
                "op":"upsert",
                "keys":["post_1"],
                "query_tags":["posts:list"],
                "schema_version":2,
                "cursor":"memory:7",
                "created_at_ms":42
            }"#,
        )
        .unwrap();

        assert_eq!(
            event,
            LiveEvent::CollectionChanged {
                collection: "posts".to_string(),
                op: LiveOp::Upsert,
                keys: vec!["post_1".to_string()],
                query_tags: vec!["posts:list".to_string()],
                schema_version: 2,
                cursor: Some("memory:7".to_string()),
                created_at_ms: Some(42),
            }
        );
    }

    #[test]
    fn parses_query_invalidated_frame() {
        let event = parse_live_event(
            KIND_QUERY_INVALIDATED,
            r#"{
                "protocol":"pocopine.live.v1",
                "query_tags":["posts:list"],
                "cursor":"memory:8",
                "created_at_ms":43
            }"#,
        )
        .unwrap();

        assert_eq!(
            event,
            LiveEvent::QueryInvalidated {
                query_tags: vec!["posts:list".to_string()],
                cursor: Some("memory:8".to_string()),
                created_at_ms: Some(43),
            }
        );
    }

    #[test]
    fn rejects_wrong_protocol_for_known_frames() {
        let err = parse_live_event(KIND_GAP, r#"{"protocol":"other","reason":"gap"}"#).unwrap_err();

        assert!(matches!(
            err,
            LiveClientError::InvalidValue {
                field: "protocol",
                ..
            }
        ));
    }

    #[test]
    fn builds_stream_url_with_collections_topics_and_cursor() {
        let url = build_live_stream_url(
            LIVE_STREAM_PATH,
            &["posts".to_string(), "comments".to_string()],
            &["query:posts:list".to_string()],
            Some("memory:12"),
        )
        .unwrap();

        assert_eq!(
            url,
            "/__pocopine/live/v1/stream?collection=posts%2Ccomments&topic=query%3Aposts%3Alist&last_event_id=memory%3A12"
        );
    }

    #[test]
    fn rejects_ambiguous_csv_values() {
        let err =
            build_live_stream_url(LIVE_STREAM_PATH, &["posts,comments".to_string()], &[], None)
                .unwrap_err();

        assert!(matches!(
            err,
            LiveClientError::InvalidValue {
                field: "collection",
                ..
            }
        ));
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::convert::Infallible;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{HeaderMap, Request, StatusCode, Uri};
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::Router;
    use futures_util::stream::{self, StreamExt};
    use pocopine_auth::RequestContext;
    use pocopine_events::{
        build_event_backend, EventBackendConfig, EventCursor, EventEnvelope, EventError,
        EventResult, LiveEventBackend, LiveEventSubscription, MemoryEventBackend,
        MemoryEventConfig, SharedEventBackend, SubscribeRequest, Topic,
    };
    #[cfg(feature = "redis")]
    use pocopine_events::{RedisEventBackend, RedisEventConfig};
    use pocopine_observe::{LOG_TARGET, TRACE_TARGET};
    use serde::Deserialize;
    use serde_json::json;

    use crate::{
        collection_topic, with_stream_metadata, LiveError, KIND_ERROR, KIND_GAP, KIND_READY,
        LIVE_PROTOCOL_V1, LIVE_STREAM_PATH,
    };

    type TopicPolicy = Arc<dyn Fn(&RequestContext, &Topic) -> bool + Send + Sync>;

    /// Live stream hub mounted by host applications.
    #[derive(Clone)]
    pub struct LiveHub<B> {
        backend: B,
        default_topics: Arc<Vec<Topic>>,
        replay_limit: usize,
        policy: TopicPolicy,
    }

    impl<B> LiveHub<B> {
        /// Build a hub. By default no topic is allowed; apps must opt in
        /// with `allow_topics`, `allow_all_topics`, or `with_topic_policy`.
        pub fn new(backend: B) -> Self {
            Self {
                backend,
                default_topics: Arc::new(Vec::new()),
                replay_limit: 100,
                policy: Arc::new(|_, _| false),
            }
        }

        /// Use these topics when the request does not specify a topic or
        /// collection.
        pub fn default_topics(mut self, topics: impl IntoIterator<Item = Topic>) -> Self {
            self.default_topics = Arc::new(topics.into_iter().collect());
            self
        }

        /// Set the opening replay limit.
        pub fn replay_limit(mut self, replay_limit: usize) -> Self {
            self.replay_limit = replay_limit;
            self
        }

        /// Allow exactly the listed topics.
        pub fn allow_topics(self, topics: impl IntoIterator<Item = Topic>) -> Self {
            let allowed = Arc::new(topics.into_iter().collect::<Vec<_>>());
            self.with_topic_policy(move |_, topic| allowed.iter().any(|allowed| allowed == topic))
        }

        /// Allow every requested topic. Apps should only use this for
        /// already-public topic sets.
        pub fn allow_all_topics(self) -> Self {
            self.with_topic_policy(|_, _| true)
        }

        /// Install an application topic policy.
        pub fn with_topic_policy(
            mut self,
            policy: impl Fn(&RequestContext, &Topic) -> bool + Send + Sync + 'static,
        ) -> Self {
            self.policy = Arc::new(policy);
            self
        }
    }

    impl LiveHub<MemoryEventBackend> {
        /// Build a live hub backed by process-local memory.
        ///
        /// This is safe for tests, development, and explicit
        /// single-process deployments. It does not coordinate multiple
        /// server processes and does not survive restarts.
        pub fn memory() -> Self {
            Self::new(MemoryEventBackend::new())
        }

        /// Build a memory-backed live hub with explicit retention.
        pub fn memory_with_config(config: MemoryEventConfig) -> EventResult<Self> {
            Ok(Self::new(MemoryEventBackend::with_config(config)?))
        }
    }

    impl LiveHub<SharedEventBackend> {
        /// Build a live hub from a supported built-in backend config.
        pub fn from_backend_config(config: EventBackendConfig) -> EventResult<Self> {
            Ok(Self::new(build_event_backend(config)?))
        }

        /// Build a live hub from an already constructed shared backend.
        pub fn shared(backend: SharedEventBackend) -> Self {
            Self::new(backend)
        }
    }

    #[cfg(feature = "redis")]
    impl LiveHub<RedisEventBackend> {
        /// Build a Redis-backed live hub from explicit config.
        pub fn redis(config: RedisEventConfig) -> EventResult<Self> {
            Ok(Self::new(RedisEventBackend::from_config(config)?))
        }

        /// Build a Redis-backed live hub from `POCOPINE_REDIS_URL`.
        pub fn redis_from_env(app: impl Into<String>) -> EventResult<Self> {
            Ok(Self::new(RedisEventBackend::from_env(app)?))
        }
    }

    /// Build Axum routes for the live SSE endpoint.
    pub fn routes<B>(hub: LiveHub<B>) -> Router
    where
        B: LiveEventBackend + Clone + Send + Sync + 'static,
    {
        Router::new()
            .route(LIVE_STREAM_PATH, get(stream_handler::<B>))
            .with_state(hub)
    }

    async fn stream_handler<B>(State(hub): State<LiveHub<B>>, request: Request<Body>) -> Response
    where
        B: LiveEventBackend + Clone + Send + Sync + 'static,
    {
        match open_sse(hub, request).await {
            Ok(sse) => sse.into_response(),
            Err(err) => err.into_response(),
        }
    }

    async fn open_sse<B>(
        hub: LiveHub<B>,
        request: Request<Body>,
    ) -> Result<
        Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>> + Send + 'static>,
        LiveError,
    >
    where
        B: LiveEventBackend + Clone + Send + Sync + 'static,
    {
        let (parts, _) = request.into_parts();
        let query = parse_query(&parts.uri)?;
        let last_event_id = last_event_id(&parts.headers).or_else(|| query.last_event_id.clone());
        let ctx =
            RequestContext::from_parts(parts.method, parts.uri, parts.headers, parts.extensions);
        let topics = hub.allowed_topics(&ctx, &query)?;
        let after = last_event_id
            .map(EventCursor::new)
            .transpose()
            .map_err(LiveError::Event)?;

        tracing::debug!(
            target: TRACE_TARGET,
            event_name = "pocopine.live.open",
            topics = topics.len(),
            replay_limit = hub.replay_limit,
        );

        let subscription = hub
            .backend
            .subscribe_live(
                SubscribeRequest::new(topics)
                    .after(after)
                    .replay_limit(hub.replay_limit),
            )
            .await?;

        Ok(sse_from_subscription(subscription).keep_alive(KeepAlive::default()))
    }

    impl<B> LiveHub<B> {
        fn allowed_topics(
            &self,
            ctx: &RequestContext,
            query: &LiveStreamQuery,
        ) -> Result<Vec<Topic>, LiveError> {
            let topics = requested_topics(query, &self.default_topics)?;
            for topic in &topics {
                if !(self.policy)(ctx, topic) {
                    tracing::warn!(
                        target: LOG_TARGET,
                        event_name = "pocopine.live.topic_denied",
                        topic = %topic,
                    );
                    return Err(LiveError::ForbiddenTopic(topic.to_string()));
                }
            }
            Ok(topics)
        }
    }

    fn sse_from_subscription(
        mut subscription: LiveEventSubscription,
    ) -> Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>> + Send + 'static> {
        let mut opening = Vec::new();
        opening.push(Ok(SseEvent::default().event(KIND_READY).data(
            json!({
                "protocol": LIVE_PROTOCOL_V1,
                "cursor": subscription
                    .opening
                    .replay
                    .cursor
                    .as_ref()
                    .map(EventCursor::as_str),
                "topics": subscription
                    .opening
                    .topics
                    .iter()
                    .map(Topic::as_str)
                    .collect::<Vec<_>>(),
            })
            .to_string(),
        )));

        if subscription.opening.replay.gap {
            opening.push(Ok(SseEvent::default().event(KIND_GAP).data(
                json!({
                    "protocol": LIVE_PROTOCOL_V1,
                    "reason": "cursor_not_replayable",
                })
                .to_string(),
            )));
        } else {
            let replay_events = std::mem::take(&mut subscription.opening.replay.events);
            opening.extend(
                replay_events
                    .into_iter()
                    .map(|event| Ok(sse_for_envelope(event))),
            );
        }

        let live = stream::unfold(subscription, |mut subscription| async move {
            match subscription.next().await {
                Ok(Some(event)) => Some((Ok(sse_for_envelope(event)), subscription)),
                Ok(None) => None,
                Err(err) => Some((Ok(sse_for_error(err)), subscription)),
            }
        });

        Sse::new(stream::iter(opening).chain(live))
    }

    fn sse_for_envelope(envelope: EventEnvelope) -> SseEvent {
        let cursor = envelope.cursor.as_str().to_string();
        let payload = with_stream_metadata(envelope.payload, &cursor, envelope.created_at_ms);
        SseEvent::default()
            .event(envelope.kind.as_str())
            .id(cursor)
            .data(payload.to_string())
    }

    fn sse_for_error(err: EventError) -> SseEvent {
        let reason = match err {
            EventError::SubscriptionLagged { .. } => "subscriber_lagged",
            _ => "event_backend_error",
        };
        SseEvent::default().event(KIND_ERROR).data(
            json!({
                "protocol": LIVE_PROTOCOL_V1,
                "reason": reason,
            })
            .to_string(),
        )
    }

    #[derive(Clone, Debug, Default, Deserialize)]
    struct LiveStreamQuery {
        #[serde(default)]
        topic: Option<String>,
        #[serde(default)]
        collection: Option<String>,
        #[serde(default)]
        last_event_id: Option<String>,
    }

    fn parse_query(uri: &Uri) -> Result<LiveStreamQuery, LiveError> {
        let Some(query) = uri.query() else {
            return Ok(LiveStreamQuery::default());
        };
        serde_urlencoded::from_str(query).map_err(|err| LiveError::BadQuery(err.to_string()))
    }

    fn requested_topics(
        query: &LiveStreamQuery,
        default_topics: &[Topic],
    ) -> Result<Vec<Topic>, LiveError> {
        let mut topics = Vec::new();
        if let Some(raw) = &query.topic {
            for topic in split_csv(raw) {
                topics.push(Topic::new(topic).map_err(LiveError::Event)?);
            }
        }
        if let Some(raw) = &query.collection {
            for collection in split_csv(raw) {
                topics.push(collection_topic(collection).map_err(LiveError::Event)?);
            }
        }
        if topics.is_empty() {
            topics.extend(default_topics.iter().cloned());
        }
        if topics.is_empty() {
            return Err(LiveError::NoTopics);
        }
        topics.sort();
        topics.dedup();
        Ok(topics)
    }

    fn split_csv(raw: &str) -> impl Iterator<Item = &str> {
        raw.split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    fn last_event_id(headers: &HeaderMap) -> Option<String> {
        headers
            .get("last-event-id")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    impl IntoResponse for LiveError {
        fn into_response(self) -> Response {
            let status = match self {
                Self::ForbiddenTopic(_) => StatusCode::FORBIDDEN,
                Self::NoTopics | Self::BadQuery(_) => StatusCode::BAD_REQUEST,
                Self::Event(_) | Self::Json(_) => StatusCode::INTERNAL_SERVER_ERROR,
            };
            let body = json!({
                "protocol": LIVE_PROTOCOL_V1,
                "error": self.to_string(),
            });
            (status, axum::Json(body)).into_response()
        }
    }

    #[cfg(test)]
    mod tests {
        use axum::http::Method;
        use pocopine_events::{
            EventBackend, EventBackendCapabilities, EventBackendConfig, EventDraft,
            MemoryEventBackend, MemoryEventConfig,
        };
        use serde_json::json;

        use super::*;
        use crate::{LiveInvalidation, KIND_COLLECTION_CHANGED};

        #[test]
        fn requested_collections_map_to_collection_topics() {
            let query = LiveStreamQuery {
                collection: Some("posts,comments".to_string()),
                ..LiveStreamQuery::default()
            };

            let topics = requested_topics(&query, &[]).unwrap();

            assert_eq!(
                topics.iter().map(Topic::as_str).collect::<Vec<_>>(),
                vec!["collection:comments", "collection:posts"]
            );
        }

        #[test]
        fn default_hub_rejects_topics() {
            let backend = MemoryEventBackend::new();
            let hub = LiveHub::new(backend);
            let ctx = RequestContext::new(Method::GET, Uri::from_static("/"), HeaderMap::new());
            let query = LiveStreamQuery {
                collection: Some("posts".to_string()),
                ..LiveStreamQuery::default()
            };

            let err = hub.allowed_topics(&ctx, &query).unwrap_err();

            assert!(matches!(err, LiveError::ForbiddenTopic(_)));
        }

        #[test]
        fn allowed_topic_policy_accepts_registered_topics() {
            let backend = MemoryEventBackend::new();
            let posts = collection_topic("posts").unwrap();
            let hub = LiveHub::new(backend).allow_topics([posts.clone()]);
            let ctx = RequestContext::new(Method::GET, Uri::from_static("/"), HeaderMap::new());
            let query = LiveStreamQuery {
                collection: Some("posts".to_string()),
                ..LiveStreamQuery::default()
            };

            let topics = hub.allowed_topics(&ctx, &query).unwrap();

            assert_eq!(topics, vec![posts]);
        }

        #[test]
        fn memory_constructor_builds_memory_hub() {
            let hub = LiveHub::memory();

            assert_eq!(
                hub.backend.capabilities(),
                EventBackendCapabilities::memory()
            );
        }

        #[test]
        fn memory_constructor_rejects_invalid_config() {
            let result = LiveHub::memory_with_config(MemoryEventConfig {
                capacity: 1,
                subscriber_capacity: 0,
            });

            assert!(matches!(result, Err(EventError::InvalidRetention(_))));
        }

        #[test]
        fn backend_config_constructor_builds_shared_hub() {
            let hub = LiveHub::from_backend_config(EventBackendConfig::memory()).unwrap();

            assert_eq!(
                hub.backend.capabilities(),
                EventBackendCapabilities::memory()
            );
        }

        #[cfg(feature = "redis")]
        #[test]
        fn redis_constructor_builds_without_connecting() {
            let config =
                pocopine_events::RedisEventConfig::new("redis://127.0.0.1/", "live-test").unwrap();
            let hub = LiveHub::redis(config).unwrap();

            assert_eq!(
                hub.backend.capabilities(),
                EventBackendCapabilities::redis_streams()
            );
        }

        #[test]
        fn invalidation_draft_uses_collection_topic_and_live_kind() {
            let draft = LiveInvalidation::upsert("posts")
                .keys(["post_1"])
                .query_tags(["posts:list"])
                .into_draft()
                .unwrap();

            assert_eq!(draft.topic.as_str(), "collection:posts");
            assert_eq!(draft.kind.as_str(), KIND_COLLECTION_CHANGED);
            assert_eq!(draft.schema_version, 1);
        }

        #[test]
        fn envelope_sse_injects_cursor_metadata() {
            let envelope = MemoryEventBackend::new()
                .publish_now(
                    EventDraft::new(
                        "collection:posts",
                        KIND_COLLECTION_CHANGED,
                        json!({
                            "collection": "posts"
                        }),
                    )
                    .unwrap(),
                )
                .unwrap();

            let event = sse_for_envelope(envelope);
            let debug = format!("{event:?}");

            assert!(debug.contains("collection.changed"));
            assert!(debug.contains("memory:1"));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use host::{routes, LiveHub};

#[cfg(target_arch = "wasm32")]
pub use browser::{
    LiveClient, LiveClientWithHandler, LiveSubscription, ScopedLiveClient,
    ScopedLiveClientWithHandler,
};
