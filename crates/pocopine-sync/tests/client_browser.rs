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
    sync_plugin, CollectionState, SyncCollectionName, SyncOpenRequest, SyncOpenResponse,
    SyncOpenShape, SyncPullRequest, SyncPullResponse, SyncRow, SyncShapeName, SYNC_OPEN_PATH,
    SYNC_PULL_PATH,
};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use web_sys::window;

wasm_bindgen_test_configure!(run_in_browser);

const SHAPE: &str = "posts_for_browser";
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
            .shape(SHAPE)
            .and_then(|collection| collection.open())
        {
            self.posts.set_error(err.to_string());
        }
    }
}

#[wasm_bindgen_test(async)]
async fn open_validates_shape_then_pull_renders_snapshot() {
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
                        assert_eq!(request.shapes[0].as_str(), SHAPE);
                        let response = SyncOpenResponse::new(vec![SyncOpenShape {
                            shape: SyncShapeName::new(SHAPE).unwrap(),
                            collection: SyncCollectionName::new(COLLECTION).unwrap(),
                            cursor: None,
                        }]);
                        Ok(json_response(response))
                    }
                    SYNC_PULL_PATH => {
                        let request: SyncPullRequest = serde_json::from_str(&req.body).unwrap();
                        assert_eq!(request.shape.as_str(), SHAPE);
                        assert!(
                            request.cursor.is_none(),
                            "the /open cursor must not make a fresh client skip its snapshot"
                        );
                        let response = SyncPullResponse::snapshot(
                            SyncShapeName::new(SHAPE).unwrap(),
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
