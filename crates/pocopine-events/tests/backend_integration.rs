#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;
#[cfg(feature = "redis")]
use std::time::{SystemTime, UNIX_EPOCH};

use pocopine_events::{
    EventBackend, EventBackendCapabilities, EventBackendConfig, EventDraft, EventError,
    EventResult, LiveEventBackend, MemoryEventBackend, ReplayRequest, SubscribeRequest, Topic,
};
use serde_json::json;

fn draft(topic: &str, n: u64) -> EventDraft {
    EventDraft::new(topic, "collection.changed", json!({ "n": n }))
        .unwrap()
        .created_at_ms(n)
}

#[tokio::test]
async fn memory_backend_publish_replay_and_live_receive() -> EventResult<()> {
    let backend = MemoryEventBackend::with_capacity(8)?;
    let posts = Topic::new("collection:posts")?;
    let comments = Topic::new("collection:comments")?;
    let mut subscription = backend
        .subscribe_live(SubscribeRequest::new([posts.clone()]))
        .await?;

    let skipped = backend.publish(draft(comments.as_str(), 1)).await?;
    let published = backend.publish(draft(posts.as_str(), 2)).await?;
    let replay = backend
        .replay(ReplayRequest::new([posts.clone()]).limit(10))
        .await?;
    let received = tokio::time::timeout(Duration::from_secs(2), subscription.next())
        .await
        .map_err(|err| EventError::Backend(format!("timed out waiting for memory event: {err}")))??
        .ok_or_else(|| EventError::Backend("memory live stream closed".into()))?;

    assert_eq!(skipped.topic, comments);
    assert_eq!(replay.events, vec![published.clone()]);
    assert_eq!(received, published);
    Ok(())
}

#[tokio::test]
async fn backend_factory_builds_shared_memory_backend() -> EventResult<()> {
    let backend = EventBackendConfig::memory().build()?;
    let topic = Topic::new("collection:posts")?;

    let published = backend.publish(draft(topic.as_str(), 1)).await?;
    let replay = backend
        .replay(ReplayRequest::new([topic]).limit(10))
        .await?;

    assert_eq!(backend.capabilities(), EventBackendCapabilities::memory());
    assert_eq!(replay.events, vec![published]);
    Ok(())
}

#[cfg(feature = "redis")]
#[tokio::test]
async fn redis_backend_publish_replay_after_cursor_and_live_receive() -> EventResult<()> {
    let Some(url) = redis_test_url() else {
        return Ok(());
    };
    let app = unique_redis_app()?;
    let config = pocopine_events::RedisEventConfig::new(url, app)?.with_stream_max_len(128)?;
    let backend = pocopine_events::RedisEventBackend::from_config(config)?;
    backend.clear_for_tests().await?;

    let posts = Topic::new("collection:posts")?;
    let mut subscription = backend
        .subscribe_live(SubscribeRequest::new([posts.clone()]))
        .await?;
    let first = backend.publish(draft(posts.as_str(), 1)).await?;
    let second = backend.publish(draft(posts.as_str(), 2)).await?;
    let replay = backend
        .replay(
            ReplayRequest::new([posts])
                .after(Some(first.cursor.clone()))
                .limit(10),
        )
        .await?;
    let received = tokio::time::timeout(Duration::from_secs(2), subscription.next())
        .await
        .map_err(|err| EventError::Backend(format!("timed out waiting for Redis event: {err}")))??
        .ok_or_else(|| EventError::Backend("Redis live stream closed".into()))?;

    assert_eq!(
        backend.capabilities(),
        EventBackendCapabilities::redis_streams()
    );
    assert_eq!(replay.events, vec![second]);
    assert_eq!(received, first);

    backend.clear_for_tests().await?;
    Ok(())
}

#[cfg(feature = "redis")]
fn redis_test_url() -> Option<String> {
    let url = match std::env::var("REDIS_TEST_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("skipping Redis event integration test: set REDIS_TEST_URL");
            return None;
        }
    };
    if url.trim().is_empty() {
        eprintln!("skipping Redis event integration test: REDIS_TEST_URL is empty");
        return None;
    }
    Some(url)
}

#[cfg(feature = "redis")]
fn unique_redis_app() -> EventResult<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|err| EventError::Time(err.to_string()))?
        .as_millis();
    Ok(format!("events-integration-{}-{now}", std::process::id()))
}
