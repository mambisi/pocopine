//! Redis-backed [`Fanout`] for multi-process fan-out (RFC 073 Phase C).
//!
//! Each topic gets its own Redis Stream plus a durable monotonic `u64`
//! counter, with the counter value used directly as the stream entry id
//! (`<seq>-0`). The counter is the source of truth for the cursor, so it
//! survives trimming, empty streams, reconnects, and a client resuming on a
//! *different* replica (the Phase C RESUME requirement).
//!
//! ```text
//! pocopine:{<app>:<topic_hash>}:ws:s   Redis Stream  (XADD <seq>-0 p <bytes>, MAXLEN ~)
//! pocopine:{<app>:<topic_hash>}:ws:c   INCR counter  (durable cursor)
//! pocopine:{<app>:<topic_hash>}:ws:w   Pub/Sub wake  (doorbell carrying the new seq)
//! ```
//!
//! The hash tag `{app:topic_hash}` co-locates a topic's three keys in one
//! Redis Cluster slot (so the publish Lua script is slot-safe) while different
//! topics distribute across slots. `publish` runs the seq assignment, `XADD`,
//! and `PUBLISH` in one atomic Lua `EVAL`. The Lua reconciles the counter
//! against the stream's top id first, so even if the counter is lost
//! independently of the stream (e.g. evicted under `maxmemory`) the explicit
//! `XADD` id is always strictly greater than the stream top and can never be
//! rejected. Live delivery is a pub/sub doorbell that triggers an `XRANGE`
//! catch-up; because durability lives in the stream, a missed doorbell
//! self-heals on the next one.
//!
//! Connections use a [`ConnectionManager`] (transparent reconnect) for all
//! XADD/XRANGE, plus one dedicated pub/sub connection PER subscription (mirrors
//! `RedisEventBackend::subscribe_live`). A shared per-fanout pub/sub dispatcher
//! that multiplexes all topics over one connection is the connection-count
//! optimization for very high subscriber counts — a documented follow-up.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::OnceLock;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use pocopine_crypto::sha256_hex;
use pocopine_events::Topic;
use redis::aio::ConnectionManager;
use redis::{AsyncCommands, Client, Script};

use super::error::WsError;
use super::fanout::{Fanout, TopicMessage, TopicSource, TopicStream};

/// Default approximate per-topic stream retention (`MAXLEN ~`).
pub const DEFAULT_REDIS_MAX_LEN: usize = 1024;

/// Environment variable holding the Redis URL (shared with `pocopine-events`).
const REDIS_URL_ENV: &str = "POCOPINE_REDIS_URL";

/// Atomic publish: assign the next seq (reconciling the counter against the
/// stream top so the explicit id is always > the stream top), append with that
/// seq as the id, trim, and ring the doorbell. KEYS = [stream, counter, wake];
/// ARGV = [max_len, payload]. Returns the new seq.
const PUBLISH_SRC: &str = r"
local topseq = 0
local top = redis.call('XREVRANGE', KEYS[1], '+', '-', 'COUNT', 1)
if top[1] then topseq = tonumber(string.match(top[1][1], '^(%d+)')) end
local seq = redis.call('INCR', KEYS[2])
if seq <= topseq then
  seq = topseq + 1
  redis.call('SET', KEYS[2], seq)
end
local id = string.format('%d-0', seq)
redis.call('XADD', KEYS[1], 'MAXLEN', '~', ARGV[1], id, 'p', ARGV[2])
redis.call('PUBLISH', KEYS[3], seq)
return seq
";

fn publish_script() -> &'static Script {
    static SCRIPT: OnceLock<Script> = OnceLock::new();
    SCRIPT.get_or_init(|| Script::new(PUBLISH_SRC))
}

type MsgStream = Pin<Box<dyn Stream<Item = redis::Msg> + Send>>;

/// XRANGE reply shape: `[(entry_id, [(field, value), ...]), ...]`.
type XRangeReply = Vec<(String, Vec<(String, Vec<u8>)>)>;

/// The three Redis keys for one topic (cluster-co-located by hash tag).
struct TopicKeys {
    stream: String,
    counter: String,
    wake: String,
}

/// Multi-process [`Fanout`] backed by per-topic Redis Streams.
#[derive(Clone)]
pub struct RedisFanout {
    client: Client,
    conn: ConnectionManager,
    app: String,
    max_len: usize,
}

impl RedisFanout {
    /// Connect to `url` under the `app` namespace.
    pub async fn connect(url: &str, app: impl Into<String>) -> Result<Self, WsError> {
        let app = app.into();
        validate_app(&app)?;
        let client =
            Client::open(url).map_err(|err| WsError::backend(format!("redis open: {err}")))?;
        // ConnectionManager transparently reconnects, so a transient drop does
        // not permanently wedge publish/subscribe.
        let conn = client
            .get_connection_manager()
            .await
            .map_err(|err| WsError::backend(format!("redis connect: {err}")))?;
        Ok(Self {
            client,
            conn,
            app,
            max_len: DEFAULT_REDIS_MAX_LEN,
        })
    }

    /// Connect using `POCOPINE_REDIS_URL` (the same env var `pocopine-events`
    /// reads). Errors loudly if it is unset — never defaults to localhost.
    pub async fn from_env(app: impl Into<String>) -> Result<Self, WsError> {
        let url = std::env::var(REDIS_URL_ENV)
            .map_err(|_| WsError::backend(format!("{REDIS_URL_ENV} is not set")))?;
        Self::connect(&url, app).await
    }

    /// Override the approximate per-topic stream retention (`MAXLEN ~`).
    pub fn with_max_len(mut self, max_len: usize) -> Self {
        self.max_len = max_len.max(1);
        self
    }

    fn keys(&self, topic: &Topic) -> TopicKeys {
        // sha256 keeps keys fixed-length and free of `{`/`}` that would break
        // the hash tag, regardless of the topic string.
        let tag = format!("{}:{}", self.app, sha256_hex(topic.as_str().as_bytes()));
        TopicKeys {
            stream: format!("pocopine:{{{tag}}}:ws:s"),
            counter: format!("pocopine:{{{tag}}}:ws:c"),
            wake: format!("pocopine:{{{tag}}}:ws:w"),
        }
    }

    /// Delete a topic's keys. Test-only helper for isolation.
    #[doc(hidden)]
    pub async fn clear_topic(&self, topic: &Topic) -> Result<(), WsError> {
        let keys = self.keys(topic);
        let mut conn = self.conn.clone();
        redis::cmd("DEL")
            .arg(&keys.stream)
            .arg(&keys.counter)
            .arg(&keys.wake)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|err| WsError::backend(format!("redis del: {err}")))?;
        Ok(())
    }

    /// Exactly trim a topic's stream to `max_len` entries. Test-only helper to
    /// force deterministic eviction (`MAXLEN ~` used in production is
    /// approximate and will not trim a small stream).
    #[doc(hidden)]
    pub async fn trim_exact(&self, topic: &Topic, max_len: usize) -> Result<(), WsError> {
        let keys = self.keys(topic);
        let mut conn = self.conn.clone();
        redis::cmd("XTRIM")
            .arg(&keys.stream)
            .arg("MAXLEN")
            .arg(max_len)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|err| WsError::backend(format!("redis xtrim: {err}")))?;
        Ok(())
    }

    /// Delete only a topic's counter key (the stream survives). Test-only
    /// helper to exercise counter/stream-divergence recovery.
    #[doc(hidden)]
    pub async fn delete_counter(&self, topic: &Topic) -> Result<(), WsError> {
        let keys = self.keys(topic);
        let mut conn = self.conn.clone();
        redis::cmd("DEL")
            .arg(&keys.counter)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|err| WsError::backend(format!("redis del counter: {err}")))?;
        Ok(())
    }
}

#[async_trait]
impl Fanout for RedisFanout {
    async fn publish(&self, topic: &Topic, payload: Bytes) -> Result<u64, WsError> {
        let keys = self.keys(topic);
        let mut conn = self.conn.clone();
        let seq: u64 = publish_script()
            .key(&keys.stream)
            .key(&keys.counter)
            .key(&keys.wake)
            .arg(self.max_len)
            .arg(payload.as_ref())
            .invoke_async(&mut conn)
            .await
            .map_err(|err| WsError::backend(format!("redis publish: {err}")))?;
        Ok(seq)
    }

    async fn subscribe(&self, topic: &Topic, after: Option<u64>) -> Result<TopicStream, WsError> {
        let keys = self.keys(topic);
        let mut conn = self.conn.clone();

        let head: u64 = conn
            .get::<_, Option<u64>>(&keys.counter)
            .await
            .map_err(|err| WsError::backend(format!("redis get counter: {err}")))?
            .unwrap_or(0);

        // A cursor past the head is unreplayable; no need to open pub/sub.
        if matches!(after, Some(a) if a > head) {
            return Ok(TopicStream::new(Box::new(GappedSource)));
        }

        // Subscribe the doorbell BEFORE the replay snapshot so any publish after
        // this point rings the bell; the replay/live overlap is de-duplicated by
        // `last_seen`.
        let mut pubsub = self
            .client
            .get_async_pubsub()
            .await
            .map_err(|err| WsError::backend(format!("redis pubsub: {err}")))?;
        pubsub
            .subscribe(&keys.wake)
            .await
            .map_err(|err| WsError::backend(format!("redis subscribe: {err}")))?;

        let start = match after {
            Some(after) => format!("({after}-0"), // exclusive: seq > after
            None => "-".to_string(),              // retained tail
        };
        let replay = xrange(&mut conn, &keys.stream, &start, "+").await?;

        // Derive the gap from the ACTUAL replay (race-safe against a trim landing
        // between a pre-read and the replay): a hole before the first replayed
        // seq, or an empty replay when entries should still exist.
        let gap = match after {
            None => false,
            Some(after) => match replay.first() {
                Some((first, _)) => *first > after.saturating_add(1),
                None => after < head,
            },
        };
        if gap {
            return Ok(TopicStream::new(Box::new(GappedSource)));
        }

        let last_seen = replay
            .last()
            .map(|(seq, _)| *seq)
            .unwrap_or(after.unwrap_or(0));
        Ok(TopicStream::new(Box::new(RedisTopicSource {
            conn,
            pubsub: Box::pin(pubsub.into_on_message()),
            stream_key: keys.stream,
            buffer: replay.into_iter().collect(),
            last_seen,
        })))
    }

    async fn trim_after(&self, topic: &Topic, durable_seq: u64) -> Result<(), WsError> {
        // Everything `<= durable_seq` is durable, so drop it: keep ids whose seq
        // is strictly greater, i.e. `MINID (durable_seq + 1)-0`. `~` lets Redis
        // trim by whole macro-nodes — it only ever keeps MORE than asked, never
        // less, so a resume at `durable_seq` stays gap-free. A `durable_seq` of 0
        // would keep everything (nothing is durable yet); skip the call.
        if durable_seq == 0 {
            return Ok(());
        }
        let min_id = format!("{}-0", durable_seq.saturating_add(1));
        let keys = self.keys(topic);
        let mut conn = self.conn.clone();
        redis::cmd("XTRIM")
            .arg(&keys.stream)
            .arg("MINID")
            .arg("~")
            .arg(&min_id)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|err| WsError::backend(format!("redis xtrim minid: {err}")))?;
        Ok(())
    }
}

/// Source for an unreplayable resume cursor: reports a gap and yields nothing.
/// The gateway sends a Gap control and never pumps it.
struct GappedSource;

#[async_trait]
impl TopicSource for GappedSource {
    async fn next(&mut self) -> Result<Option<TopicMessage>, WsError> {
        Ok(None)
    }

    fn gap(&self) -> bool {
        true
    }
}

/// Live per-subscription [`TopicSource`]: a drained replay buffer then a
/// pub/sub doorbell that triggers `XRANGE` catch-ups.
struct RedisTopicSource {
    conn: ConnectionManager,
    pubsub: MsgStream,
    stream_key: String,
    buffer: VecDeque<TopicMessage>,
    last_seen: u64,
}

#[async_trait]
impl TopicSource for RedisTopicSource {
    fn gap(&self) -> bool {
        false
    }

    async fn next(&mut self) -> Result<Option<TopicMessage>, WsError> {
        loop {
            if let Some(msg) = self.buffer.pop_front() {
                return Ok(Some(msg));
            }
            match self.pubsub.next().await {
                // A closed doorbell is a connection LOSS, not a clean shutdown
                // (the pub/sub connection does not auto-reconnect). Surface it so
                // the gateway sends a Gap and the client re-subscribes from its
                // durable cursor — distinct from the gateway closing the socket.
                None => return Err(WsError::backend("redis pub/sub doorbell closed")),
                Some(msg) => {
                    // The doorbell payload is the published seq; skip the XRANGE
                    // if we already have everything up to it.
                    if let Ok(woke) = msg.get_payload::<u64>()
                        && woke <= self.last_seen
                    {
                        continue;
                    }
                    let start = format!("({}-0", self.last_seen);
                    let fresh = xrange(&mut self.conn, &self.stream_key, &start, "+").await?;
                    // Eviction under a slow live subscriber: a hole between
                    // last_seen and the first fetched seq means the tail lost
                    // ordering. Surface it as a lag so the client re-subscribes.
                    if let Some((first, _)) = fresh.first()
                        && *first > self.last_seen.saturating_add(1)
                    {
                        return Err(WsError::Lagged(first - self.last_seen - 1));
                    }
                    for (seq, bytes) in fresh {
                        self.last_seen = self.last_seen.max(seq);
                        self.buffer.push_back((seq, bytes));
                    }
                }
            }
        }
    }
}

/// XRANGE `stream` over `[start, end]`, parsing each `<seq>-0` id and the `p`
/// payload field into `(seq, bytes)`.
async fn xrange(
    conn: &mut ConnectionManager,
    stream: &str,
    start: &str,
    end: &str,
) -> Result<Vec<TopicMessage>, WsError> {
    let reply: XRangeReply = redis::cmd("XRANGE")
        .arg(stream)
        .arg(start)
        .arg(end)
        .query_async(conn)
        .await
        .map_err(|err| WsError::backend(format!("redis xrange: {err}")))?;
    let mut out = Vec::with_capacity(reply.len());
    for (id, fields) in reply {
        let seq = parse_seq(&id)?;
        let payload = fields
            .into_iter()
            .find(|(field, _)| field == "p")
            .map(|(_, value)| Bytes::from(value))
            .ok_or_else(|| WsError::backend(format!("redis entry {id} missing 'p' field")))?;
        out.push((seq, payload));
    }
    Ok(out)
}

fn parse_seq(id: &str) -> Result<u64, WsError> {
    id.split('-')
        .next()
        .and_then(|seq| seq.parse::<u64>().ok())
        .ok_or_else(|| WsError::backend(format!("redis: malformed stream id {id}")))
}

fn validate_app(app: &str) -> Result<(), WsError> {
    if app.is_empty() || app.contains(['{', '}']) {
        return Err(WsError::backend(format!(
            "invalid redis app namespace {app:?}: must be non-empty and free of braces"
        )));
    }
    Ok(())
}
