#![cfg(pocopine_host)]

use http_body_util::BodyExt;
use pocopine_live::{LIVE_STREAM_PATH, LiveHub, build_live_stream_url, routes};
use pocopine_server::Server;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_sync::{
    ClientMutation, MutationId, RowKey, SYNC_PULL_PATH, SYNC_PUSH_PATH, SyncOp, SyncPullMode,
    SyncPullRequest, SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncStreamName,
    sync_server_plugin, sync_stream_tag,
};
use sync_example::{POSTS_STREAM, Post, live_backend, sync_server};
use tower::ServiceExt;

#[tokio::test]
async fn sync_pull_and_live_wakeup_share_the_stream_topic() {
    pocopine_server::__reset_for_test();
    let sync = sync_server();
    let sync_topic_prefixes = sync.live_topic_prefixes();
    let default_topics = sync.live_topics().unwrap();
    let app = Server::new(routes(
        LiveHub::new(live_backend())
            .allow_topic_prefixes(sync_topic_prefixes)
            .default_topics(default_topics),
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

    let pushed = push_post(
        &app,
        Post {
            id: "post_from_push".to_string(),
            title: "CI sync wake-up".to_string(),
            body: "incremental pull should receive this row".to_string(),
        },
    )
    .await;
    assert_eq!(pushed.accepted[0].as_str(), "ci:post_from_push");
    assert!(pushed.rejected.is_empty());
    assert!(pushed.conflicts.is_empty());

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

async fn push_post(app: &pocopine_server::axum::Router, post: Post) -> SyncPushResponse<Post> {
    let request = SyncPushRequest::new(
        SyncStreamName::new(POSTS_STREAM).unwrap(),
        [ClientMutation {
            id: MutationId::new(format!("ci:{}", post.id)).unwrap(),
            key: Some(RowKey::new(post.id.clone()).unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: post,

            migration_outcome: None,
        }],
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(SYNC_PUSH_PATH)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&request).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let outer: pocopine::ServerResult<SyncPushResponse<Post>> =
        serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
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
