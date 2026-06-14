//! Browser smoke test for the sync client.
//!
//! Run with:
//!   `wasm-pack test --firefox --headless crates/pocopine-sync --test client_browser`

#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Promise;
use pocopine::prelude::*;
use pocopine_sync::{
    ClientMutation, ClientMutationDraft, CollectionState, LocalPendingMutation, LocalSnapshotBatch,
    MemoryLocalStore, MutationId, RowKey, SYNC_OPEN_PATH, SYNC_PULL_PATH, SYNC_PUSH_PATH,
    SyncChange, SyncCollectionName, SyncCursor, SyncDeviceId, SyncLocalIdentity, SyncLocalStore,
    SyncOp, SyncOpenRequest, SyncOpenResponse, SyncOpenStream, SyncPullRequest, SyncPullResponse,
    SyncPushRequest, SyncPushResponse, SyncRow, SyncStreamName, sync_plugin,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

wasm_bindgen_test_configure!(run_in_browser);

const STREAM: &str = "posts_for_browser";
const COLLECTION: &str = "posts";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct BrowserPost {
    id: String,
    title: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "sync-browser-board",
    template_inline = r#"
    <section class="sync-browser-board">
      <output class="row-count" pp-text="posts.rows.length"></output>
      <p class="error" pp-text="posts.error"></p>
      <ol class="posts">
        <template pp-for="row in posts.rows" pp-key="row.key">
          <li class="post" pp-text="row.value.title"></li>
        </template>
      </ol>
    </section>
    "#
)]
struct SyncBrowserBoard {
    posts: CollectionState<BrowserPost>,
}

#[handlers]
impl SyncBrowserBoard {
    pub fn on_mount(&mut self) {
        if let Err(err) = self
            .plugin::<pocopine_sync::SyncClient>()
            .collection(pocopine::this::<Self>(), |state: &mut Self| {
                &mut state.posts
            })
            .stream(STREAM)
            .and_then(|collection| collection.open())
        {
            self.posts.set_error(err.to_string());
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "sync-browser-push-board",
    template_inline = r#"
    <section class="sync-browser-push-board">
      <p class="error" pp-text="posts.error"></p>
      <ol class="posts">
        <template pp-for="row in posts.rows" pp-key="row.key">
          <li class="post" pp-text="row.value.title"></li>
        </template>
      </ol>
    </section>
    "#
)]
struct SyncBrowserPushBoard {
    posts: CollectionState<BrowserPost>,
}

#[handlers]
impl SyncBrowserPushBoard {
    pub fn on_mount(&mut self) {
        let post = BrowserPost {
            id: "post_generated".to_string(),
            title: "Generated id push".to_string(),
        };
        let result = ClientMutationDraft::upsert(post.clone())
            .key(post.id.clone())
            .map(|mutation| {
                (
                    mutation,
                    SyncRow::new(post.id.clone(), post)
                        .expect("test optimistic row should be valid"),
                )
            })
            .and_then(|(mutation, optimistic)| {
                self.plugin::<pocopine_sync::SyncClient>()
                    .collection(pocopine::this::<Self>(), |state: &mut Self| {
                        &mut state.posts
                    })
                    .stream(STREAM)
                    .and_then(|collection| {
                        collection.push_with_generated_id(mutation, Some(optimistic))
                    })
            });

        if let Err(err) = result {
            self.posts.set_error(err.to_string());
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    name = "sync-browser-online-push-board",
    template_inline = r#"
    <section class="sync-browser-online-push-board">
      <p class="error" pp-text="posts.error"></p>
      <ol class="posts">
        <template pp-for="row in posts.rows" pp-key="row.key">
          <li class="post" pp-text="row.value.title"></li>
        </template>
      </ol>
    </section>
    "#
)]
struct SyncBrowserOnlinePushBoard {
    posts: CollectionState<BrowserPost>,
}

#[handlers]
impl SyncBrowserOnlinePushBoard {
    pub fn on_mount(&mut self) {
        let post = BrowserPost {
            id: "post_online".to_string(),
            title: "Online-only push".to_string(),
        };
        let result = ClientMutationDraft::upsert(post.clone())
            .key(post.id.clone())
            .map(|mutation| {
                (
                    mutation,
                    SyncRow::new(post.id.clone(), post)
                        .expect("test optimistic row should be valid"),
                )
            })
            .and_then(|(mutation, optimistic)| {
                self.plugin::<pocopine_sync::SyncClient>()
                    .collection(pocopine::this::<Self>(), |state: &mut Self| {
                        &mut state.posts
                    })
                    .stream(STREAM)
                    .and_then(|collection| {
                        collection.push_with_generated_id_online(mutation, Some(optimistic))
                    })
            });

        if let Err(err) = result {
            self.posts.set_error(err.to_string());
        }
    }
}

#[wasm_bindgen_test(async)]
async fn open_validates_stream_then_pull_renders_snapshot() {
    pocopine::fetch::__reset_middleware_chain_for_test();

    let seen_urls = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen_urls_for_middleware = seen_urls.clone();
    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let seen_urls = seen_urls_for_middleware.clone();
            async move {
                seen_urls.borrow_mut().push(req.url.clone());
                match req.url.as_str() {
                    SYNC_OPEN_PATH => {
                        let request: SyncOpenRequest = serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.streams[0].stream.as_str(), STREAM);
                        assert!(
                            request.streams[0].params.is_empty(),
                            "default subscription must serialize with empty params (RFC-085 backwards-compat)"
                        );
                        let response = SyncOpenResponse::new(vec![SyncOpenStream {
                            stream: SyncStreamName::new(STREAM).unwrap(),
                            collection: SyncCollectionName::new(COLLECTION).unwrap(),
                            cursor: None,
                            schema_version: 1,
                        }]);
                        Ok(json_response(response))
                    }
                    SYNC_PULL_PATH => {
                        let request: SyncPullRequest = serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.stream.as_str(), STREAM);
                        assert!(
                            request.cursor.is_none(),
                            "the /open cursor must not make a fresh client skip its snapshot"
                        );
                        let response = SyncPullResponse::snapshot(
                            SyncStreamName::new(STREAM).unwrap(),
                            SyncCollectionName::new(COLLECTION).unwrap(),
                            vec![
                                SyncRow::new(
                                    "post_1",
                                    BrowserPost {
                                        id: "post_1".to_string(),
                                        title: "Loaded through sync".to_string(),
                                    },
                                )
                                .unwrap(),
                            ],
                            Some(pocopine_sync::SyncCursor::new("1").unwrap()),
                        );
                        Ok(json_response(response))
                    }
                    other => Err(pocopine::ServerError::Network(format!(
                        "unexpected sync browser test request: {other}"
                    ))),
                }
            }
        },
    );

    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<sync-browser-board></sync-browser-board>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(sync_plugin().with_live_wakeup(false))
        .register::<SyncBrowserBoard>()
        .run();

    settle().await;

    assert_eq!(
        &*seen_urls.borrow(),
        &[SYNC_OPEN_PATH.to_string(), SYNC_PULL_PATH.to_string()],
        "sync client should call /open before the first /pull"
    );
    let post = host
        .query_selector(".post")
        .unwrap()
        .expect("synced post should render");
    assert_eq!(
        post.text_content().unwrap_or_default(),
        "Loaded through sync"
    );
    assert_eq!(
        host.query_selector(".row-count")
            .unwrap()
            .unwrap()
            .text_content()
            .unwrap_or_default(),
        "1"
    );

    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[wasm_bindgen_test(async)]
async fn open_hydrates_local_store_and_pulls_from_cached_cursor() {
    pocopine::fetch::__reset_middleware_chain_for_test();

    let store = MemoryLocalStore::new();
    store
        .save_snapshot(LocalSnapshotBatch::new(
            SyncStreamName::new(STREAM).unwrap(),
            SyncCollectionName::new(COLLECTION).unwrap(),
            vec![
                SyncRow::new(
                    "post_1",
                    serde_json::json!({
                        "id": "post_1",
                        "title": "Cached locally"
                    }),
                )
                .unwrap(),
            ],
            Some(SyncCursor::new("cached_cursor").unwrap()),
        ))
        .await
        .unwrap();

    let seen_pull_cursor = Rc::new(RefCell::new(None::<String>));
    let seen_pull_cursor_for_middleware = seen_pull_cursor.clone();
    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let seen_pull_cursor = seen_pull_cursor_for_middleware.clone();
            async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => {
                        let request: SyncOpenRequest = serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.streams[0].stream.as_str(), STREAM);
                        assert!(
                            request.streams[0].params.is_empty(),
                            "default subscription must serialize with empty params (RFC-085 backwards-compat)"
                        );
                        let response = SyncOpenResponse::new(vec![SyncOpenStream {
                            stream: SyncStreamName::new(STREAM).unwrap(),
                            collection: SyncCollectionName::new(COLLECTION).unwrap(),
                            cursor: None,
                            schema_version: 1,
                        }]);
                        Ok(json_response(response))
                    }
                    SYNC_PULL_PATH => {
                        let request: SyncPullRequest = serde_json::from_str(&req.body).unwrap();
                        *seen_pull_cursor.borrow_mut() =
                            request.cursor.as_ref().map(ToString::to_string);
                        assert_eq!(request.cursor.as_ref().unwrap().as_str(), "cached_cursor");

                        let response = SyncPullResponse::incremental(
                            SyncStreamName::new(STREAM).unwrap(),
                            SyncCollectionName::new(COLLECTION).unwrap(),
                            vec![SyncChange {
                                stream: SyncStreamName::new(STREAM).unwrap(),
                                collection: SyncCollectionName::new(COLLECTION).unwrap(),
                                key: Some(pocopine_sync::RowKey::new("post_2").unwrap()),
                                op: SyncOp::Upsert,
                                row: Some(
                                    SyncRow::new(
                                        "post_2",
                                        BrowserPost {
                                            id: "post_2".to_string(),
                                            title: "Loaded incrementally".to_string(),
                                        },
                                    )
                                    .unwrap(),
                                ),
                                cursor: SyncCursor::new("cursor_2").unwrap(),
                            }],
                            Some(SyncCursor::new("cursor_2").unwrap()),
                        );
                        Ok(json_response(response))
                    }
                    other => Err(pocopine::ServerError::Network(format!(
                        "unexpected sync browser test request: {other}"
                    ))),
                }
            }
        },
    );

    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<sync-browser-board></sync-browser-board>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(
            sync_plugin()
                .with_live_wakeup(false)
                .local_store(store.clone()),
        )
        .register::<SyncBrowserBoard>()
        .run();

    settle().await;

    assert_eq!(seen_pull_cursor.borrow().as_deref(), Some("cached_cursor"));
    assert_eq!(
        host.query_selector(".row-count")
            .unwrap()
            .unwrap()
            .text_content()
            .unwrap_or_default(),
        "2"
    );
    let persisted = store
        .hydrate_stream(&SyncStreamName::new(STREAM).unwrap())
        .await
        .unwrap();
    assert_eq!(persisted.cursor.unwrap().as_str(), "cursor_2");
    assert_eq!(persisted.rows.len(), 2);

    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[wasm_bindgen_test(async)]
async fn open_replays_pending_mutations_before_pull() {
    pocopine::fetch::__reset_middleware_chain_for_test();

    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new(STREAM).unwrap();
    let collection = SyncCollectionName::new(COLLECTION).unwrap();

    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection.clone(),
            vec![
                SyncRow::new(
                    "post_1",
                    serde_json::json!({"id": "post_1", "title": "Cached"}),
                )
                .unwrap(),
            ],
            Some(SyncCursor::new("cached_cursor").unwrap()),
        ))
        .await
        .unwrap();

    let pending = ClientMutation {
        id: MutationId::new("device_browser:42").unwrap(),
        key: Some(RowKey::new("post_2").unwrap()),
        op: SyncOp::Upsert,
        base_version: None,
        payload: serde_json::json!({"id": "post_2", "title": "Pending"}),

        migration_outcome: None,
    };
    store.enqueue_mutation(&stream, pending).await.unwrap();

    let push_seen = Rc::new(RefCell::new(0u32));
    let pull_seen = Rc::new(RefCell::new(0u32));
    let pull_cursor_seen = Rc::new(RefCell::new(None::<String>));
    let push_seen_for_mw = push_seen.clone();
    let pull_seen_for_mw = pull_seen.clone();
    let pull_cursor_for_mw = pull_cursor_seen.clone();

    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let push_seen = push_seen_for_mw.clone();
            let pull_seen = pull_seen_for_mw.clone();
            let pull_cursor_seen = pull_cursor_for_mw.clone();
            async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => {
                        Ok(json_response(SyncOpenResponse::new(vec![SyncOpenStream {
                            stream: SyncStreamName::new(STREAM).unwrap(),
                            collection: SyncCollectionName::new(COLLECTION).unwrap(),
                            cursor: None,
                            schema_version: 1,
                        }])))
                    }
                    SYNC_PUSH_PATH => {
                        *push_seen.borrow_mut() += 1;
                        let request: SyncPushRequest<serde_json::Value> =
                            serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.mutations.len(), 1);
                        assert_eq!(request.mutations[0].id.as_str(), "device_browser:42");
                        assert_eq!(
                            request.mutations[0].key.as_ref().unwrap().as_str(),
                            "post_2"
                        );

                        let mut response = SyncPushResponse::<BrowserPost>::new(
                            SyncStreamName::new(STREAM).unwrap(),
                        );
                        response.collection = Some(SyncCollectionName::new(COLLECTION).unwrap());
                        response
                            .accepted
                            .push(MutationId::new("device_browser:42").unwrap());
                        response.rows.push(
                            SyncRow::new(
                                "post_2",
                                BrowserPost {
                                    id: "post_2".to_string(),
                                    title: "Confirmed".to_string(),
                                },
                            )
                            .unwrap(),
                        );
                        response.cursor = Some(SyncCursor::new("after_replay").unwrap());
                        Ok(json_response(response))
                    }
                    SYNC_PULL_PATH => {
                        *pull_seen.borrow_mut() += 1;
                        let request: SyncPullRequest = serde_json::from_str(&req.body).unwrap();
                        *pull_cursor_seen.borrow_mut() =
                            request.cursor.as_ref().map(ToString::to_string);
                        let response = SyncPullResponse::<BrowserPost>::incremental(
                            SyncStreamName::new(STREAM).unwrap(),
                            SyncCollectionName::new(COLLECTION).unwrap(),
                            Vec::new(),
                            Some(SyncCursor::new("after_pull").unwrap()),
                        );
                        Ok(json_response(response))
                    }
                    other => Err(pocopine::ServerError::Network(format!(
                        "unexpected sync browser test request: {other}"
                    ))),
                }
            }
        },
    );

    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<sync-browser-board></sync-browser-board>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(
            sync_plugin()
                .with_live_wakeup(false)
                .local_store(store.clone()),
        )
        .register::<SyncBrowserBoard>()
        .run();

    settle().await;

    assert_eq!(
        *push_seen.borrow(),
        1,
        "open should replay one pending mutation before pulling"
    );
    assert_eq!(*pull_seen.borrow(), 1, "pull runs once after replay");
    assert_eq!(
        pull_cursor_seen.borrow().as_deref(),
        Some("cached_cursor"),
        "pull continues from the cached cursor that was loaded before replay"
    );

    let persisted = store.hydrate_stream(&stream).await.unwrap();
    assert!(
        persisted.pending_mutations.is_empty(),
        "accepted mutations should be cleared from the queue"
    );
    assert_eq!(persisted.cursor.unwrap().as_str(), "after_pull");
    assert_eq!(
        persisted.rows.len(),
        2,
        "local store should hold cached + replay-confirmed rows"
    );

    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[wasm_bindgen_test(async)]
async fn open_keeps_hydrated_pending_overlay_when_replay_fails() {
    pocopine::fetch::__reset_middleware_chain_for_test();

    let store = MemoryLocalStore::new();
    let stream = SyncStreamName::new(STREAM).unwrap();
    let collection = SyncCollectionName::new(COLLECTION).unwrap();

    store
        .save_snapshot(LocalSnapshotBatch::new(
            stream.clone(),
            collection,
            vec![
                SyncRow::new(
                    "post_1",
                    serde_json::json!({"id": "post_1", "title": "Cached"}),
                )
                .unwrap(),
            ],
            Some(SyncCursor::new("cached_cursor").unwrap()),
        ))
        .await
        .unwrap();

    store
        .enqueue_pending_mutation(
            &stream,
            LocalPendingMutation::new(ClientMutation {
                id: MutationId::new("device_browser:pending").unwrap(),
                key: Some(RowKey::new("post_2").unwrap()),
                op: SyncOp::Upsert,
                base_version: None,
                payload: serde_json::json!({
                    "op": "create",
                    "payload": {"id": "post_2", "draft": {"title": "Envelope only"}}
                }),

                migration_outcome: None,
            })
            .with_optimistic_row(Some(
                SyncRow::new(
                    "post_2",
                    serde_json::json!({"id": "post_2", "title": "Pending local"}),
                )
                .unwrap(),
            )),
        )
        .await
        .unwrap();

    let push_seen = Rc::new(RefCell::new(0u32));
    let pull_seen = Rc::new(RefCell::new(0u32));
    let push_seen_for_mw = push_seen.clone();
    let pull_seen_for_mw = pull_seen.clone();

    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let push_seen = push_seen_for_mw.clone();
            let pull_seen = pull_seen_for_mw.clone();
            async move {
                match req.url.as_str() {
                    SYNC_OPEN_PATH => {
                        Ok(json_response(SyncOpenResponse::new(vec![SyncOpenStream {
                            stream: SyncStreamName::new(STREAM).unwrap(),
                            collection: SyncCollectionName::new(COLLECTION).unwrap(),
                            cursor: None,
                            schema_version: 1,
                        }])))
                    }
                    SYNC_PUSH_PATH => {
                        *push_seen.borrow_mut() += 1;
                        let request: SyncPushRequest<serde_json::Value> =
                            serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.mutations.len(), 1);
                        assert_eq!(request.mutations[0].id.as_str(), "device_browser:pending");
                        Err(pocopine::ServerError::Network("offline replay".to_string()))
                    }
                    SYNC_PULL_PATH => {
                        *pull_seen.borrow_mut() += 1;
                        let request: SyncPullRequest = serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.cursor.as_ref().unwrap().as_str(), "cached_cursor");
                        Ok(json_response(SyncPullResponse::<BrowserPost>::incremental(
                            SyncStreamName::new(STREAM).unwrap(),
                            SyncCollectionName::new(COLLECTION).unwrap(),
                            Vec::new(),
                            Some(SyncCursor::new("after_pull").unwrap()),
                        )))
                    }
                    other => Err(pocopine::ServerError::Network(format!(
                        "unexpected sync browser test request: {other}"
                    ))),
                }
            }
        },
    );

    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<sync-browser-board></sync-browser-board>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(
            sync_plugin()
                .with_live_wakeup(false)
                .local_store(store.clone()),
        )
        .register::<SyncBrowserBoard>()
        .run();

    settle().await;

    assert_eq!(*push_seen.borrow(), 1);
    assert_eq!(*pull_seen.borrow(), 1);

    let posts = host.query_selector_all(".post").unwrap();
    assert_eq!(posts.length(), 2);
    assert_eq!(
        posts.item(0).unwrap().text_content().unwrap_or_default(),
        "Cached"
    );
    assert_eq!(
        posts.item(1).unwrap().text_content().unwrap_or_default(),
        "Pending local"
    );
    let error = host
        .query_selector(".error")
        .unwrap()
        .unwrap()
        .text_content()
        .unwrap_or_default();
    assert!(error.contains("pending sync mutation replay failed"));
    assert!(error.contains("offline replay"));

    let persisted = store.hydrate_stream(&stream).await.unwrap();
    assert_eq!(persisted.pending_mutations.len(), 1);
    assert_eq!(
        persisted.pending_mutations[0].mutation.id.as_str(),
        "device_browser:pending"
    );
    assert!(persisted.pending_mutations[0].optimistic_row.is_some());

    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[wasm_bindgen_test(async)]
async fn push_with_generated_id_reserves_identity_before_enqueue() {
    pocopine::fetch::__reset_middleware_chain_for_test();

    let store = MemoryLocalStore::new();
    store
        .save_identity(
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_browser").unwrap(), 42)
                .unwrap(),
        )
        .await
        .unwrap();

    let pushed_id = Rc::new(RefCell::new(None::<String>));
    let pushed_id_for_mw = pushed_id.clone();
    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let pushed_id = pushed_id_for_mw.clone();
            async move {
                match req.url.as_str() {
                    SYNC_PUSH_PATH => {
                        let request: SyncPushRequest<BrowserPost> =
                            serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.mutations.len(), 1);
                        let mutation = &request.mutations[0];
                        *pushed_id.borrow_mut() = Some(mutation.id.to_string());
                        assert_eq!(mutation.id.as_str(), "device_browser:42");
                        assert_eq!(mutation.key.as_ref().unwrap().as_str(), "post_generated");

                        let mut response = SyncPushResponse::<BrowserPost>::new(
                            SyncStreamName::new(STREAM).unwrap(),
                        );
                        response.collection = Some(SyncCollectionName::new(COLLECTION).unwrap());
                        response
                            .accepted
                            .push(MutationId::new("device_browser:42").unwrap());
                        response.rows.push(
                            SyncRow::new(
                                "post_generated",
                                BrowserPost {
                                    id: "post_generated".to_string(),
                                    title: "Confirmed generated id".to_string(),
                                },
                            )
                            .unwrap(),
                        );
                        response.cursor = Some(SyncCursor::new("after_push").unwrap());
                        Ok(json_response(response))
                    }
                    SYNC_PULL_PATH => {
                        Ok(json_response(SyncPullResponse::<BrowserPost>::incremental(
                            SyncStreamName::new(STREAM).unwrap(),
                            SyncCollectionName::new(COLLECTION).unwrap(),
                            Vec::new(),
                            Some(SyncCursor::new("after_pull").unwrap()),
                        )))
                    }
                    other => Err(pocopine::ServerError::Network(format!(
                        "unexpected sync browser test request: {other}"
                    ))),
                }
            }
        },
    );

    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<sync-browser-push-board></sync-browser-push-board>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(
            sync_plugin()
                .with_live_wakeup(false)
                .local_store(store.clone()),
        )
        .register::<SyncBrowserPushBoard>()
        .run();

    settle().await;

    assert_eq!(pushed_id.borrow().as_deref(), Some("device_browser:42"));
    assert_eq!(
        store
            .load_identity()
            .await
            .unwrap()
            .unwrap()
            .next_mutation_counter,
        43
    );
    let persisted = store
        .hydrate_stream(&SyncStreamName::new(STREAM).unwrap())
        .await
        .unwrap();
    assert!(persisted.pending_mutations.is_empty());
    assert_eq!(persisted.rows[0].value["title"], "Confirmed generated id");

    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[wasm_bindgen_test(async)]
async fn online_push_failure_does_not_queue_pending_mutation() {
    pocopine::fetch::__reset_middleware_chain_for_test();

    let store = MemoryLocalStore::new();
    store
        .save_identity(
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_browser").unwrap(), 42)
                .unwrap(),
        )
        .await
        .unwrap();

    let push_seen = Rc::new(RefCell::new(0u32));
    let push_seen_for_mw = push_seen.clone();
    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| {
            let push_seen = push_seen_for_mw.clone();
            async move {
                match req.url.as_str() {
                    SYNC_PUSH_PATH => {
                        *push_seen.borrow_mut() += 1;
                        let request: SyncPushRequest<BrowserPost> =
                            serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.mutations.len(), 1);
                        assert_eq!(request.mutations[0].id.as_str(), "device_browser:42");
                        Err(pocopine::ServerError::Network(
                            "offline in online-only test".to_string(),
                        ))
                    }
                    other => Err(pocopine::ServerError::Network(format!(
                        "unexpected sync browser test request: {other}"
                    ))),
                }
            }
        },
    );

    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<sync-browser-online-push-board></sync-browser-online-push-board>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(
            sync_plugin()
                .with_live_wakeup(false)
                .local_store(store.clone()),
        )
        .register::<SyncBrowserOnlinePushBoard>()
        .run();

    settle().await;

    assert_eq!(*push_seen.borrow(), 1);
    assert_eq!(
        store
            .load_identity()
            .await
            .unwrap()
            .unwrap()
            .next_mutation_counter,
        43,
        "online-only pushes still reserve durable ids and may skip them"
    );
    let persisted = store
        .hydrate_stream(&SyncStreamName::new(STREAM).unwrap())
        .await
        .unwrap();
    assert!(
        persisted.pending_mutations.is_empty(),
        "online-only failure must not queue a replayable mutation"
    );
    assert!(
        persisted.rows.is_empty(),
        "online-only optimistic rows are not persisted as local cache rows"
    );
    assert!(
        host.query_selector(".post").unwrap().is_none(),
        "failed online-only optimistic row should roll back from component state"
    );
    let error = host
        .query_selector(".error")
        .unwrap()
        .unwrap()
        .text_content()
        .unwrap_or_default();
    assert!(error.contains("offline in online-only test"));

    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

#[wasm_bindgen_test(async)]
async fn online_push_success_updates_state_without_persisting_local_outcome() {
    pocopine::fetch::__reset_middleware_chain_for_test();

    let store = MemoryLocalStore::new();
    store
        .save_identity(
            SyncLocalIdentity::with_next_counter(SyncDeviceId::new("device_browser").unwrap(), 42)
                .unwrap(),
        )
        .await
        .unwrap();

    pocopine::fetch::install_middleware(
        move |req: pocopine::fetch::FetchRequest, _next: pocopine::fetch::FetchNext| async move {
            match req.url.as_str() {
                SYNC_PUSH_PATH => {
                    let request: SyncPushRequest<BrowserPost> =
                        serde_json::from_str(&req.body).unwrap();
                    assert_eq!(request.mutations.len(), 1);
                    assert_eq!(request.mutations[0].id.as_str(), "device_browser:42");

                    let mut response =
                        SyncPushResponse::<BrowserPost>::new(SyncStreamName::new(STREAM).unwrap());
                    response.collection = Some(SyncCollectionName::new(COLLECTION).unwrap());
                    response
                        .accepted
                        .push(MutationId::new("device_browser:42").unwrap());
                    response.rows.push(
                        SyncRow::new(
                            "post_online",
                            BrowserPost {
                                id: "post_online".to_string(),
                                title: "Confirmed online-only".to_string(),
                            },
                        )
                        .unwrap(),
                    );
                    Ok(json_response(response))
                }
                other => Err(pocopine::ServerError::Network(format!(
                    "unexpected sync browser test request: {other}"
                ))),
            }
        },
    );

    let document = window().unwrap().document().unwrap();
    let host = document.create_element("div").unwrap();
    host.set_attribute("pp-app", "").unwrap();
    host.set_inner_html("<sync-browser-online-push-board></sync-browser-online-push-board>");
    document.body().unwrap().append_child(&host).unwrap();

    App::new()
        .plugin(
            sync_plugin()
                .with_live_wakeup(false)
                .local_store(store.clone()),
        )
        .register::<SyncBrowserOnlinePushBoard>()
        .run();

    settle().await;

    let post = host
        .query_selector(".post")
        .unwrap()
        .expect("confirmed online-only row should render");
    assert_eq!(
        post.text_content().unwrap_or_default(),
        "Confirmed online-only"
    );

    let persisted = store
        .hydrate_stream(&SyncStreamName::new(STREAM).unwrap())
        .await
        .unwrap();
    assert!(persisted.pending_mutations.is_empty());
    assert!(
        persisted.rows.is_empty(),
        "online-only success updates component state but does not write local-store outcomes"
    );

    host.remove();
    pocopine::fetch::__reset_middleware_chain_for_test();
}

fn json_response<T: Serialize>(value: T) -> pocopine::fetch::FetchResponse {
    pocopine::fetch::FetchResponse {
        status: 200,
        body: serde_json::to_string(&Ok::<T, pocopine::ServerError>(value)).unwrap(),
    }
}

async fn settle() {
    for _ in 0..4 {
        next_task().await;
    }
}

async fn next_task() {
    let promise = Promise::new(&mut |resolve, _reject| {
        if let Some(window) = window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0);
        } else {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        }
    });
    let _ = JsFuture::from(promise).await;
}
