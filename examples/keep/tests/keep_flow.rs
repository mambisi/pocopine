#![cfg(pocopine_host)]

use http_body_util::BodyExt;
use keep_example::{KEEP_STREAM, KEEP_TAGS_STREAM, KeepNote, KeepTag, live_backend, sync_server};
use pocopine_live::{LIVE_STREAM_PATH, LiveHub, build_live_stream_url, routes};
use pocopine_server::Server;
use pocopine_server::axum::body::Body;
use pocopine_server::axum::http::{Request, StatusCode};
use pocopine_sync::{
    ClientMutation, MutationId, RowKey, SYNC_PULL_PATH, SYNC_PUSH_PATH, SyncOp, SyncPullMode,
    SyncPullRequest, SyncPullResponse, SyncPushRequest, SyncPushResponse, SyncStreamName,
    sync_server_plugin, sync_stream_tag,
};
use tower::ServiceExt;

#[tokio::test]
async fn keep_push_and_live_wakeup_share_the_stream_topic() {
    pocopine_server::__reset_for_test();
    let test_path =
        std::env::temp_dir().join(format!("pocopine_keep_flow_{}.sqlite3", std::process::id()));
    let test_tags_path = std::env::temp_dir().join(format!(
        "pocopine_keep_tags_flow_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&test_path);
    let _ = std::fs::remove_file(&test_tags_path);
    // SAFETY: test-only; set before the server reads it.
    unsafe { std::env::set_var("POCOPINE_KEEP_NOTES_DB_PATH", &test_path) };
    // SAFETY: test-only; set before the server reads it.
    unsafe { std::env::set_var("POCOPINE_KEEP_TAGS_DB_PATH", &test_tags_path) };

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

    let first = pull_notes(&app, None).await;
    assert_eq!(first.mode, SyncPullMode::Snapshot);
    assert!(
        first.rows.is_empty(),
        "the keep example seeds nothing — the demo workspace should start blank"
    );
    let first_tags = pull_tags(&app, None).await;
    assert_eq!(first_tags.mode, SyncPullMode::Snapshot);
    assert!(
        first_tags.rows.is_empty(),
        "the keep example should start with no placeholder tags"
    );

    let stream_url = build_live_stream_url(
        LIVE_STREAM_PATH,
        &[],
        &[format!("query:{}", sync_stream_tag(KEEP_STREAM))],
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
    assert!(ready.contains("query:sync:stream:keep_notes_for_user"));

    let pushed = push_note(
        &app,
        KeepNote {
            id: "note_from_push".to_string(),
            title: "CI note".to_string(),
            body: "incremental pull should receive this row".to_string(),
            body_state: None,
            color: "sky".to_string(),
            pinned: false,
            archived: false,
            todos: Vec::new(),
            labels: vec!["ci".to_string()],
            updated_at_ms: 9,
        },
    )
    .await;
    assert_eq!(pushed.accepted[0].as_str(), "ci:note_from_push");
    assert!(pushed.rejected.is_empty());
    assert!(pushed.conflicts.is_empty());

    let invalidation = read_sse_frame(&mut body).await;
    assert!(invalidation.contains("event: query.invalidated"));
    assert!(invalidation.contains("sync:stream:keep_notes_for_user"));

    let second = pull_notes(&app, first.cursor).await;
    assert_eq!(second.mode, SyncPullMode::Incremental);
    assert!(second.changes.iter().any(|change| {
        change
            .row
            .as_ref()
            .map(|row| row.value.title == "CI note")
            .unwrap_or(false)
    }));

    let pushed_tag = push_tag(
        &app,
        KeepTag {
            id: "ci".to_string(),
            name: "ci".to_string(),
            updated_at_ms: 10,
        },
    )
    .await;
    assert_eq!(pushed_tag.accepted[0].as_str(), "ci:tag:ci");
    assert!(pushed_tag.rejected.is_empty());
    assert!(pushed_tag.conflicts.is_empty());

    let second_tags = pull_tags(&app, first_tags.cursor).await;
    assert_eq!(second_tags.mode, SyncPullMode::Incremental);
    assert!(second_tags.changes.iter().any(|change| {
        change
            .row
            .as_ref()
            .map(|row| row.value.name == "ci")
            .unwrap_or(false)
    }));

    let _ = std::fs::remove_file(&test_path);
    let _ = std::fs::remove_file(&test_tags_path);
}

async fn push_note(
    app: &pocopine_server::axum::Router,
    note: KeepNote,
) -> SyncPushResponse<KeepNote> {
    let request = SyncPushRequest::new(
        SyncStreamName::new(KEEP_STREAM).unwrap(),
        [ClientMutation {
            id: MutationId::new(format!("ci:{}", note.id)).unwrap(),
            key: Some(RowKey::new(note.id.clone()).unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: note,

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
    let outer: pocopine::ServerResult<SyncPushResponse<KeepNote>> =
        serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}

async fn push_tag(app: &pocopine_server::axum::Router, tag: KeepTag) -> SyncPushResponse<KeepTag> {
    let request = SyncPushRequest::new(
        SyncStreamName::new(KEEP_TAGS_STREAM).unwrap(),
        [ClientMutation {
            id: MutationId::new(format!("ci:tag:{}", tag.id)).unwrap(),
            key: Some(RowKey::new(tag.id.clone()).unwrap()),
            op: SyncOp::Upsert,
            base_version: None,
            payload: tag,

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
    let outer: pocopine::ServerResult<SyncPushResponse<KeepTag>> =
        serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}

async fn pull_notes(
    app: &pocopine_server::axum::Router,
    cursor: Option<pocopine_sync::SyncCursor>,
) -> SyncPullResponse<KeepNote> {
    let request = SyncPullRequest::new(SyncStreamName::new(KEEP_STREAM).unwrap()).cursor(cursor);
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
    let outer: pocopine::ServerResult<SyncPullResponse<KeepNote>> =
        serde_json::from_slice(&bytes).unwrap();
    outer.unwrap()
}

async fn pull_tags(
    app: &pocopine_server::axum::Router,
    cursor: Option<pocopine_sync::SyncCursor>,
) -> SyncPullResponse<KeepTag> {
    let request =
        SyncPullRequest::new(SyncStreamName::new(KEEP_TAGS_STREAM).unwrap()).cursor(cursor);
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
    let outer: pocopine::ServerResult<SyncPullResponse<KeepTag>> =
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
