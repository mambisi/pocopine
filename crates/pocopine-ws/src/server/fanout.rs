//! Fan-out abstraction and the in-process implementation (RFC 073 §9–§10.2).
//!
//! [`Fanout`] is the seam between the gateway and the message bus. For each
//! topic it must: assign a topic-local monotonic cursor on publish, replay
//! retained messages after a given cursor (for [`crate::Control::Resume`]),
//! signal a gap when a resume cursor is no longer retained, and deliver live
//! messages to subscribers.
//!
//! [`LocalFanout`] is the single-process implementation: a per-topic ring
//! buffer (for sequencing + replay) plus a `tokio::sync::broadcast` channel
//! (for live delivery). The multi-process implementation backed by the
//! `pocopine-events` Redis spine is the RFC 073 Phase C follow-up and will
//! implement this same trait — there `seq` is gateway-assigned and the opaque
//! `EventCursor` is mapped to it, whereas here the cursor *is* the seq.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use pocopine_events::Topic;
use tokio::sync::broadcast;

use super::error::WsError;

/// Default per-topic ring/broadcast capacity.
pub const DEFAULT_TOPIC_CAPACITY: usize = 1024;

/// A delivered fan-out message: a topic-local monotonic cursor (>= 1) and the
/// opaque payload bytes.
pub type TopicMessage = (u64, Bytes);

/// The message-bus seam the gateway fans out over.
#[async_trait]
pub trait Fanout: Send + Sync + 'static {
    /// Append `payload` to `topic`'s log, returning the assigned topic-local
    /// monotonic cursor (>= 1).
    async fn publish(&self, topic: &Topic, payload: Bytes) -> Result<u64, WsError>;

    /// Subscribe to `topic`, replaying retained messages strictly after
    /// `after` (`None` = a fresh subscribe; deliver retained tail then live).
    /// The returned [`TopicStream`] yields replayed messages first, then live
    /// ones, with no gap or duplication across the boundary.
    async fn subscribe(&self, topic: &Topic, after: Option<u64>) -> Result<TopicStream, WsError>;
}

/// A per-topic message stream: retained replay followed by live broadcast.
pub struct TopicStream {
    replay: std::vec::IntoIter<TopicMessage>,
    gap: bool,
    rx: broadcast::Receiver<TopicMessage>,
}

impl TopicStream {
    /// True if the requested resume cursor was older than retention, so a hole
    /// exists before the replayed messages. The client must re-subscribe fresh
    /// (and, for collab, re-run the sync handshake).
    pub fn gap(&self) -> bool {
        self.gap
    }

    /// Return the next message: drained replay first, then live broadcast.
    /// `Ok(None)` means the topic channel closed; `Err(WsError::Lagged)` means
    /// the live subscription fell behind and lost ordering.
    pub async fn next(&mut self) -> Result<Option<TopicMessage>, WsError> {
        if let Some(msg) = self.replay.next() {
            return Ok(Some(msg));
        }
        match self.rx.recv().await {
            Ok(msg) => Ok(Some(msg)),
            Err(broadcast::error::RecvError::Closed) => Ok(None),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Err(WsError::Lagged(skipped)),
        }
    }
}

/// Per-topic append-only ring buffer assigning monotonic sequence numbers.
struct TopicLog {
    next_seq: u64,
    capacity: usize,
    ring: VecDeque<TopicMessage>,
}

impl TopicLog {
    fn new(capacity: usize) -> Self {
        Self {
            next_seq: 1,
            capacity: capacity.max(1),
            ring: VecDeque::new(),
        }
    }

    fn append(&mut self, payload: Bytes) -> TopicMessage {
        let seq = self.next_seq;
        self.next_seq += 1;
        let msg = (seq, payload);
        self.ring.push_back(msg.clone());
        while self.ring.len() > self.capacity {
            self.ring.pop_front();
        }
        msg
    }

    fn earliest_seq(&self) -> Option<u64> {
        self.ring.front().map(|(seq, _)| *seq)
    }

    /// Returns `(replay, gap)`. A fresh subscribe (`None`) replays the retained
    /// tail with no gap. A resume (`Some(after)`) replays messages with seq
    /// strictly greater than `after`, flagging a gap if the cursor is no longer
    /// replayable: either a seq in `(after, earliest)` was evicted, or the
    /// cursor is ahead of any seq we have ever issued (a stale/forged cursor).
    fn replay_after(&self, after: Option<u64>) -> (Vec<TopicMessage>, bool) {
        match after {
            None => (self.ring.iter().cloned().collect(), false),
            Some(after) => {
                // `head` is the last seq assigned (0 if none). A cursor past it
                // is unreplayable — and this early return also avoids any
                // overflow on `after.saturating_add(1)` for huge cursors.
                let head = self.next_seq.saturating_sub(1);
                if after > head {
                    return (Vec::new(), true);
                }
                let events = self
                    .ring
                    .iter()
                    .filter(|(seq, _)| *seq > after)
                    .cloned()
                    .collect();
                let gap = match self.earliest_seq() {
                    Some(earliest) => earliest > after.saturating_add(1),
                    None => false,
                };
                (events, gap)
            }
        }
    }
}

struct TopicChannel {
    log: TopicLog,
    tx: broadcast::Sender<TopicMessage>,
}

impl TopicChannel {
    fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self {
            log: TopicLog::new(capacity),
            tx,
        }
    }
}

/// Single-process [`Fanout`]. Topics are created lazily on first publish or
/// subscribe and never removed (matching `pocopine-events`'s per-process
/// memory backend, which also retains topic state for the process lifetime).
pub struct LocalFanout {
    capacity: usize,
    topics: Mutex<HashMap<Topic, TopicChannel>>,
}

impl LocalFanout {
    /// A fan-out with the default per-topic capacity.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_TOPIC_CAPACITY)
    }

    /// A fan-out whose per-topic ring buffer and broadcast channel hold
    /// `capacity` messages.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            topics: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for LocalFanout {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Fanout for LocalFanout {
    async fn publish(&self, topic: &Topic, payload: Bytes) -> Result<u64, WsError> {
        // std::Mutex is held without crossing an .await — safe in async.
        let mut topics = self.topics.lock().expect("fanout mutex poisoned");
        let channel = topics
            .entry(topic.clone())
            .or_insert_with(|| TopicChannel::new(self.capacity));
        let msg = channel.log.append(payload);
        let seq = msg.0;
        // Ignore the "no live receivers" error — the ring still retains it.
        let _ = channel.tx.send(msg);
        Ok(seq)
    }

    async fn subscribe(&self, topic: &Topic, after: Option<u64>) -> Result<TopicStream, WsError> {
        // Snapshot replay and subscribe to the broadcast under the SAME lock so
        // no publish can slip between the two (no missed or duplicated message
        // across the replay/live boundary).
        let mut topics = self.topics.lock().expect("fanout mutex poisoned");
        let channel = topics
            .entry(topic.clone())
            .or_insert_with(|| TopicChannel::new(self.capacity));
        let (events, gap) = channel.log.replay_after(after);
        let rx = channel.tx.subscribe();
        Ok(TopicStream {
            replay: events.into_iter(),
            gap,
            rx,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(name: &str) -> Topic {
        Topic::new(name).expect("valid topic")
    }

    fn bytes(b: &[u8]) -> Bytes {
        Bytes::copy_from_slice(b)
    }

    #[test]
    fn topic_log_assigns_monotonic_seqs() {
        let mut log = TopicLog::new(8);
        assert_eq!(log.append(bytes(b"a")).0, 1);
        assert_eq!(log.append(bytes(b"b")).0, 2);
        assert_eq!(log.append(bytes(b"c")).0, 3);
    }

    #[test]
    fn topic_log_replay_fresh_returns_tail() {
        let mut log = TopicLog::new(8);
        log.append(bytes(b"a"));
        log.append(bytes(b"b"));
        let (events, gap) = log.replay_after(None);
        assert!(!gap);
        assert_eq!(
            events.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn topic_log_replay_after_filters() {
        let mut log = TopicLog::new(8);
        for b in [b"a", b"b", b"c"] {
            log.append(bytes(b));
        }
        let (events, gap) = log.replay_after(Some(1));
        assert!(!gap);
        assert_eq!(
            events.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![2, 3]
        );
    }

    #[test]
    fn topic_log_eviction_drops_oldest() {
        let mut log = TopicLog::new(2);
        for b in [b"a", b"b", b"c"] {
            log.append(bytes(b));
        }
        // Ring holds seqs 2,3 now (capacity 2).
        assert_eq!(log.earliest_seq(), Some(2));
    }

    #[test]
    fn topic_log_gap_when_resume_cursor_evicted() {
        let mut log = TopicLog::new(2);
        for b in [b"a", b"b", b"c", b"d"] {
            log.append(bytes(b));
        }
        // Ring holds seqs 3,4. Resuming after seq 1 has a hole (seq 2 gone).
        let (_, gap) = log.replay_after(Some(1));
        assert!(gap);
        // Resuming after seq 3 is fine (4 is retained).
        let (events, gap) = log.replay_after(Some(3));
        assert!(!gap);
        assert_eq!(events.iter().map(|(s, _)| *s).collect::<Vec<_>>(), vec![4]);
    }

    #[test]
    fn topic_log_caught_up_resume_is_empty_no_gap() {
        let mut log = TopicLog::new(8);
        log.append(bytes(b"a"));
        let (events, gap) = log.replay_after(Some(1));
        assert!(!gap);
        assert!(events.is_empty());
    }

    #[test]
    fn topic_log_future_cursor_reports_gap() {
        let mut log = TopicLog::new(8);
        log.append(bytes(b"a"));
        log.append(bytes(b"b"));
        // Client claims seq 5 but only 1,2 were ever issued — unreplayable.
        let (events, gap) = log.replay_after(Some(5));
        assert!(gap);
        assert!(events.is_empty());
    }

    #[test]
    fn topic_log_max_cursor_does_not_panic() {
        let mut log = TopicLog::new(8);
        log.append(bytes(b"a"));
        // u64::MAX cursor must not overflow `after + 1`; it is just a gap.
        let (events, gap) = log.replay_after(Some(u64::MAX));
        assert!(gap);
        assert!(events.is_empty());
    }

    #[tokio::test]
    async fn publish_then_subscribe_replays() {
        let fanout = LocalFanout::new();
        let topic = topic("collab:abc");
        assert_eq!(fanout.publish(&topic, bytes(b"one")).await.unwrap(), 1);
        assert_eq!(fanout.publish(&topic, bytes(b"two")).await.unwrap(), 2);

        let mut stream = fanout.subscribe(&topic, None).await.unwrap();
        assert!(!stream.gap());
        assert_eq!(stream.next().await.unwrap(), Some((1, bytes(b"one"))));
        assert_eq!(stream.next().await.unwrap(), Some((2, bytes(b"two"))));
    }

    #[tokio::test]
    async fn subscribe_then_publish_delivers_live() {
        let fanout = LocalFanout::new();
        let topic = topic("collab:abc");
        let mut stream = fanout.subscribe(&topic, None).await.unwrap();
        fanout.publish(&topic, bytes(b"live")).await.unwrap();
        assert_eq!(stream.next().await.unwrap(), Some((1, bytes(b"live"))));
    }

    #[tokio::test]
    async fn replay_and_live_are_contiguous() {
        let fanout = LocalFanout::new();
        let topic = topic("collab:abc");
        fanout.publish(&topic, bytes(b"1")).await.unwrap();
        let mut stream = fanout.subscribe(&topic, None).await.unwrap();
        fanout.publish(&topic, bytes(b"2")).await.unwrap();
        // Replayed seq 1, then live seq 2 — no gap, no duplicate.
        assert_eq!(stream.next().await.unwrap().unwrap().0, 1);
        assert_eq!(stream.next().await.unwrap().unwrap().0, 2);
    }

    #[tokio::test]
    async fn resume_after_eviction_reports_gap() {
        let fanout = LocalFanout::with_capacity(2);
        let topic = topic("collab:abc");
        for b in [b"a", b"b", b"c", b"d"] {
            fanout.publish(&topic, bytes(b)).await.unwrap();
        }
        let stream = fanout.subscribe(&topic, Some(1)).await.unwrap();
        assert!(stream.gap());
    }

    #[tokio::test]
    async fn overflowed_live_subscriber_lags() {
        let fanout = LocalFanout::with_capacity(2);
        let topic = topic("collab:abc");
        // Subscribe (no replay), then overflow the broadcast buffer before
        // reading.
        let mut stream = fanout.subscribe(&topic, None).await.unwrap();
        for b in [b"a", b"b", b"c", b"d", b"e"] {
            fanout.publish(&topic, bytes(b)).await.unwrap();
        }
        match stream.next().await {
            Err(WsError::Lagged(_)) => {}
            other => panic!("expected Lagged, got {other:?}"),
        }
    }
}
