//! Integration tests for the Redis-backed Fanout (RFC 073 Phase C).
//!
//! Gated on `REDIS_TEST_URL` (the `pocopine-events` idiom): when it is unset
//! each test prints a skip notice and returns, so CI stays green without a
//! Redis. To run locally:
//!
//! ```sh
//! docker run --rm -p 6379:6379 redis:7-alpine
//! REDIS_TEST_URL=redis://127.0.0.1:6379/ cargo test -p pocopine-ws --features redis --test redis_fanout
//! ```

#![cfg(all(feature = "redis", not(target_arch = "wasm32")))]

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use pocopine_events::Topic;
use pocopine_ws::{Fanout, RedisFanout, TopicStream, WsError};

/// Return the test Redis URL, or `None` (with a skip notice) when unset.
fn redis_url() -> Option<String> {
    match std::env::var("REDIS_TEST_URL") {
        Ok(url) if !url.is_empty() => Some(url),
        _ => {
            eprintln!("skipping redis fanout test: set REDIS_TEST_URL");
            None
        }
    }
}

/// A process-unique app namespace so concurrent tests (and reruns) never share
/// Redis keys.
fn unique_app() -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "wstest{}_{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

fn topic(name: &str) -> Topic {
    Topic::new(name).expect("valid topic")
}

fn bytes(b: &[u8]) -> Bytes {
    Bytes::copy_from_slice(b)
}

/// Await the next message with a timeout so a hang fails loudly.
async fn next(stream: &mut TopicStream) -> Option<(u64, Bytes)> {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream.next timed out")
        .expect("stream error")
}

macro_rules! redis_test {
    ($name:ident, $url:ident, $body:block) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $name() {
            let Some($url) = redis_url() else { return };
            $body
        }
    };
}

redis_test!(publish_assigns_monotonic_seq, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    assert_eq!(fanout.publish(&t, bytes(b"a")).await.unwrap(), 1);
    assert_eq!(fanout.publish(&t, bytes(b"b")).await.unwrap(), 2);
    assert_eq!(fanout.publish(&t, bytes(b"c")).await.unwrap(), 3);
});

redis_test!(publish_then_subscribe_replays_tail, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    fanout.publish(&t, bytes(b"one")).await.unwrap();
    fanout.publish(&t, bytes(b"two")).await.unwrap();

    let mut stream = fanout.subscribe(&t, None).await.unwrap();
    assert!(!stream.gap());
    assert_eq!(next(&mut stream).await, Some((1, bytes(b"one"))));
    assert_eq!(next(&mut stream).await, Some((2, bytes(b"two"))));
});

redis_test!(subscribe_then_publish_delivers_live, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    let mut stream = fanout.subscribe(&t, None).await.unwrap();
    // Publish after the subscription's pub/sub doorbell is registered.
    fanout.publish(&t, bytes(b"live")).await.unwrap();
    assert_eq!(next(&mut stream).await, Some((1, bytes(b"live"))));
});

redis_test!(replay_then_live_is_contiguous, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    fanout.publish(&t, bytes(b"1")).await.unwrap();
    let mut stream = fanout.subscribe(&t, None).await.unwrap();
    fanout.publish(&t, bytes(b"2")).await.unwrap();
    // Replayed seq 1, then live seq 2 — no gap, no duplicate.
    assert_eq!(next(&mut stream).await.unwrap().0, 1);
    assert_eq!(next(&mut stream).await.unwrap().0, 2);
});

// The headline Phase-C property: a DIFFERENT connection (a different replica)
// resumes after a durable cursor, because the seq lives in Redis, not in the
// publishing process.
redis_test!(resume_across_connections_replays_after_cursor, url, {
    let app = unique_app();
    let t = topic("doc:1");

    let writer = RedisFanout::connect(&url, &app).await.unwrap();
    writer.publish(&t, bytes(b"m1")).await.unwrap();
    writer.publish(&t, bytes(b"m2")).await.unwrap();
    drop(writer); // the publishing "replica" goes away

    let reader = RedisFanout::connect(&url, &app).await.unwrap();
    let mut stream = reader.subscribe(&t, Some(1)).await.unwrap();
    assert!(!stream.gap());
    let m = next(&mut stream).await.unwrap();
    assert_eq!(m.0, 2, "resume replays only seq > last_seen");
    assert_eq!(m.1.as_ref(), b"m2");
});

redis_test!(evicted_cursor_reports_gap, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    for b in [b"a", b"b", b"c", b"d"] {
        fanout.publish(&t, bytes(b)).await.unwrap();
    }
    // Force an exact trim so seq 1,2 are gone, keeping 3,4 (production's
    // `MAXLEN ~` is approximate and would not trim such a small stream).
    fanout.trim_exact(&t, 2).await.unwrap();
    // Resuming after seq 1 now needs an evicted entry → gap.
    let stream = fanout.subscribe(&t, Some(1)).await.unwrap();
    assert!(stream.gap());
});

redis_test!(future_cursor_reports_gap, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    fanout.publish(&t, bytes(b"a")).await.unwrap();
    // Client claims seq 99 but only 1 was issued — unreplayable.
    let stream = fanout.subscribe(&t, Some(99)).await.unwrap();
    assert!(stream.gap());
});

redis_test!(binary_payloads_round_trip, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    let payload = bytes(&[0u8, 1, 2, 255, 254, 0, 42]);
    fanout.publish(&t, payload.clone()).await.unwrap();
    let mut stream = fanout.subscribe(&t, None).await.unwrap();
    assert_eq!(next(&mut stream).await, Some((1, payload)));
});

redis_test!(topics_are_isolated, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let a = topic("doc:a");
    let b = topic("doc:b");
    fanout.publish(&a, bytes(b"a1")).await.unwrap();
    fanout.publish(&b, bytes(b"b1")).await.unwrap();
    // Each topic has its own seq starting at 1 and its own payload.
    let mut sa = fanout.subscribe(&a, None).await.unwrap();
    assert_eq!(next(&mut sa).await, Some((1, bytes(b"a1"))));
    let mut sb = fanout.subscribe(&b, None).await.unwrap();
    assert_eq!(next(&mut sb).await, Some((1, bytes(b"b1"))));
});

// Counter/stream divergence recovery: if the durable counter is lost (e.g.
// evicted under maxmemory) while the stream survives, publish must reconcile
// rather than wedge on a rejected XADD id.
redis_test!(publish_recovers_after_counter_loss, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    assert_eq!(fanout.publish(&t, bytes(b"a")).await.unwrap(), 1);
    assert_eq!(fanout.publish(&t, bytes(b"b")).await.unwrap(), 2);
    assert_eq!(fanout.publish(&t, bytes(b"c")).await.unwrap(), 3);
    fanout.delete_counter(&t).await.unwrap();
    assert_eq!(
        fanout.publish(&t, bytes(b"d")).await.unwrap(),
        4,
        "seq reconciled to stream top + 1 after counter loss"
    );
});

// A live subscriber that falls behind retention must surface Lagged (not
// silently deliver a discontinuous sequence) so the client re-subscribes.
redis_test!(live_subscriber_lagged_on_eviction, url, {
    let fanout = RedisFanout::connect(&url, unique_app()).await.unwrap();
    let t = topic("doc:1");
    let mut stream = fanout.subscribe(&t, None).await.unwrap(); // last_seen = 0
    for b in [b"a", b"b", b"c", b"d"] {
        fanout.publish(&t, bytes(b)).await.unwrap();
    }
    // Evict seq 1,2 from under the still-at-0 subscriber.
    fanout.trim_exact(&t, 2).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream.next timed out");
    match result {
        Err(WsError::Lagged(_)) => {}
        other => panic!("expected Lagged, got {other:?}"),
    }
});
