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
    sync_plugin, CollectionState, LocalSnapshotBatch, MemoryLocalStore, SyncChange,
    SyncCollectionName, SyncCursor, SyncLocalStore, SyncOp, SyncOpenRequest, SyncOpenResponse,
    SyncOpenStream, SyncPullRequest, SyncPullResponse, SyncRow, SyncStreamName, SYNC_OPEN_PATH,
    SYNC_PULL_PATH,
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
                        assert_eq!(request.streams[0].as_str(), STREAM);
                        let response = SyncOpenResponse::new(vec![SyncOpenStream {
                            stream: SyncStreamName::new(STREAM).unwrap(),
                            collection: SyncCollectionName::new(COLLECTION).unwrap(),
                            cursor: None,
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
                            vec![SyncRow::new(
                                "post_1",
                                BrowserPost {
                                    id: "post_1".to_string(),
                                    title: "Loaded through sync".to_string(),
                                },
                            )
                            .unwrap()],
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
            vec![SyncRow::new(
                "post_1",
                serde_json::json!({
                    "id": "post_1",
                    "title": "Cached locally"
                }),
            )
            .unwrap()],
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
                        assert_eq!(request.streams[0].as_str(), STREAM);
                        let response = SyncOpenResponse::new(vec![SyncOpenStream {
                            stream: SyncStreamName::new(STREAM).unwrap(),
                            collection: SyncCollectionName::new(COLLECTION).unwrap(),
                            cursor: None,
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
