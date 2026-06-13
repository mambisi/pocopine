#![cfg(pocopine_host)]

use http_body_util::BodyExt;
use live_example::{POSTS_COLLECTION, POSTS_LIST_QUERY_TAG, PostDraft, create_post, live_backend};
use pocopine::live::{
    LIVE_STREAM_PATH, LiveHub, build_live_stream_url, collection_topic, query_tag_topic, routes,
};
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn query_tag_stream_receives_post_create_invalidation() {
    let posts_topic = collection_topic(POSTS_COLLECTION).unwrap();
    let posts_list_topic = query_tag_topic(POSTS_LIST_QUERY_TAG).unwrap();
    let app = routes(
        LiveHub::new(live_backend())
            .allow_topics([posts_topic.clone(), posts_list_topic.clone()])
            .default_topics([posts_topic, posts_list_topic]),
    );

    let stream_url = build_live_stream_url(
        LIVE_STREAM_PATH,
        &[],
        &[format!("query:{POSTS_LIST_QUERY_TAG}")],
        None,
    )
    .unwrap();

    let response = app
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
    assert!(ready.contains("query:posts:list"));

    create_post(PostDraft {
        title: "CI live refresh".to_string(),
        body: "query-tag subscribers should refetch after this post".to_string(),
    })
    .await
    .unwrap();

    let invalidation = read_sse_frame(&mut body).await;
    assert!(invalidation.contains("event: query.invalidated"));
    assert!(invalidation.contains(r#""query_tags":["posts:list"]"#));
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
