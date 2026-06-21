use std::collections::HashMap;
use std::sync::Arc;

use pocopine_core::ServerResult;
use pocopine_events::SharedEventBackend;
use pocopine_server::auth::{Predicate, RequestContext};

use super::*;
use crate::{SyncError, SyncResult, sync_stream_tag};

#[derive(Clone)]
pub(crate) struct RegisteredSyncStream {
    pub(crate) source: Arc<dyn SyncStreamSource>,
    pub(crate) guard: Option<Arc<dyn SyncStreamGuard>>,
}

impl RegisteredSyncStream {
    pub(crate) async fn authorize(&self, ctx: RequestContext) -> ServerResult<()> {
        if let Some(guard) = &self.guard {
            guard.check(ctx).await
        } else {
            Ok(())
        }
    }
}

#[derive(Clone)]
pub(crate) struct SyncServerInner {
    pub(crate) streams: Arc<HashMap<String, Arc<RegisteredSyncStream>>>,
    pub(crate) events: Option<SharedEventBackend>,
}

/// Host-side sync server service.
#[derive(Clone)]
pub struct SyncServer {
    pub(crate) inner: Arc<SyncServerInner>,
}

impl SyncServer {
    /// Start building a sync server.
    pub fn builder() -> SyncServerBuilder {
        SyncServerBuilder::default()
    }

    /// Return the live query tags this server can publish for streams.
    pub fn live_query_tags(&self) -> Vec<String> {
        self.inner
            .streams
            .keys()
            .map(|stream| sync_stream_tag(stream))
            .collect()
    }

    /// Return the live topics this server can publish for streams.
    pub fn live_topics(&self) -> pocopine_events::EventResult<Vec<pocopine_events::Topic>> {
        self.live_query_tags()
            .into_iter()
            .map(|tag| pocopine_live::query_tag_topic(&tag))
            .collect()
    }

    /// Return the live topic PREFIXES this server publishes against
    /// for each registered stream. Each prefix matches the bare
    /// topic AND every RFC 088 §C per-`(stream, params_hash)`
    /// variant: `query:sync:stream:{name}` covers `query:sync:stream:{name}`
    /// and `query:sync:stream:{name}:abc…`.
    ///
    /// Pair with [`pocopine_live::LiveHub::allow_topic_prefixes`]
    /// to permit clients to subscribe to per-params topics that the
    /// exact-match `allow_topics` couldn't accept (the hashes are
    /// computed at runtime, so the allowlist must be a prefix).
    pub fn live_topic_prefixes(&self) -> Vec<String> {
        self.live_query_tags()
            .into_iter()
            .map(|tag| format!("query:{tag}"))
            .collect()
    }

    pub(crate) fn stream(&self, stream: &str) -> SyncResult<Arc<RegisteredSyncStream>> {
        self.inner
            .streams
            .get(stream)
            .cloned()
            .ok_or_else(|| SyncError::UnknownStream(stream.to_string()))
    }
}

/// Builder for [`SyncServer`].
#[derive(Default)]
pub struct SyncServerBuilder {
    streams: HashMap<String, Arc<RegisteredSyncStream>>,
    events: Option<SharedEventBackend>,
}

impl SyncServerBuilder {
    /// Register one explicitly public stream source.
    pub fn public_stream<S>(mut self, stream: S) -> Self
    where
        S: SyncStreamSource,
    {
        self.insert_stream(stream, None);
        self
    }

    /// Register one stream guarded by a sync auth predicate.
    pub fn guarded_stream<S, P>(mut self, stream: S, predicate: P) -> Self
    where
        S: SyncStreamSource,
        P: Predicate,
    {
        self.insert_stream(stream, Some(Arc::new(PredicateStreamGuard(predicate))));
        self
    }

    /// Register one stream guarded by an async request-context guard.
    pub fn guarded_stream_with<S, G>(mut self, stream: S, guard: G) -> Self
    where
        S: SyncStreamSource,
        G: SyncStreamGuard,
    {
        self.insert_stream(stream, Some(Arc::new(guard)));
        self
    }

    /// Attach the event backend shared with `pocopine-live`.
    pub fn events(mut self, events: SharedEventBackend) -> Self {
        self.events = Some(events);
        self
    }

    /// Finish the sync server.
    pub fn build(self) -> SyncServer {
        SyncServer {
            inner: Arc::new(SyncServerInner {
                streams: Arc::new(self.streams),
                events: self.events,
            }),
        }
    }

    fn insert_stream<S>(&mut self, stream: S, guard: Option<Arc<dyn SyncStreamGuard>>)
    where
        S: SyncStreamSource,
    {
        self.streams.insert(
            stream.stream().as_str().to_string(),
            Arc::new(RegisteredSyncStream {
                source: Arc::new(stream),
                guard,
            }),
        );
    }
}
