#![cfg(pocopine_host)]

use http_body_util::BodyExt;
use pocopine_live::{build_live_stream_url, routes, LiveHub, LIVE_STREAM_PATH};
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_server::Server;
use pocopine_sync::{
    sync_server_plugin, sync_stream_tag, SyncPullMode, SyncPullRequest, SyncPullResponse,
    SyncStreamName, SYNC_PULL_PATH,
};
use sync_example::{create_post, live_backend, sync_server, Post, POSTS_STREAM};
use tower::ServiceExt;

#[tokio::test]
async fn sync_pull_and_live_wakeup_share_the_stream_topic() {
    pocopine_server::__reset_for_test();
    let sync = sync_server();
    let sync_topics = sync.live_topics().unwrap();
    let app = Server::new(routes(
        LiveHub::new(live_backend())
            .allow_topics(sync_topics.clone())
            .default_topics(sync_topics),
    ))
    .plugin(sync_server_plugin(sync))
    .try_finalize()
    .unwrap();

    let first = pull_posts(&app, None).await;
    assert_eq!(first.mode, SyncPullMode::Snapshot);
    assert!(first.rows.iter().any(|row| row.key.as_str() == "post_1"));

    let stream_url = build_live_stream_url(
        LIVE_STREAM_PATH,
        &[],
        &[format!("query:{}", sync_stream_tag(POSTS_STREAM))],
        None,
    )
    .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(stream_url)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let ready = read_sse_frame(&mut body).await;
    assert!(ready.contains("event: ready"));
    assert!(ready.contains("query:sync:stream:posts_for_user"));

    create_post(
        "CI sync wake-up".to_string(),
        "incremental pull should receive this row".to_string(),
    )
    .await
    .unwrap();

    let invalidation = read_sse_frame(&mut body).await;
    assert!(invalidation.contains("event: query.invalidated"));
    assert!(invalidation.contains("sync:stream:posts_for_user"));

    let second = pull_posts(&app, first.cursor).await;
    assert_eq!(second.mode, SyncPullMode::Incremental);
    assert!(second.changes.iter().any(|change| {
        change
            .row
            .as_ref()
            .map(|row| row.value.title == "CI sync wake-up")
            .unwrap_or(false)
    }));
}

async fn pull_posts(
    app: &pocopine_server::axum::Router,
    cursor: Option<pocopine_sync::SyncCursor>,
) -> SyncPullResponse<Post> {
    let request = SyncPullRequest::new(SyncStreamName::new(POSTS_STREAM).unwrap()).cursor(cursor);
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(SYNC_PULL_PATH)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine::ServerResult<SyncPullResponse<Post>> =
        serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}

async fn read_sse_frame(body: &mut Body) -> String {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
        .await
        .expect("timed out waiting for SSE frame")
        .expect("SSE body ended before expected frame")
        .expect("SSE body returned an error");
    let data = frame.into_data().expect("SSE frame should contain data");
    std::str::from_utf8(&data)
        .expect("SSE data should be utf-8")
        .to_string()
}
