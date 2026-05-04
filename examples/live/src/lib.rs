use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(pocopine_host)]
use {
    pocopine_events::{EventBackend, MemoryEventBackend},
    std::sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    pub title: String,
    pub body: String,
    pub version: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PostDraft {
    pub title: String,
    pub body: String,
}

#[cfg(pocopine_host)]
static POSTS: OnceLock<Mutex<Vec<Post>>> = OnceLock::new();
#[cfg(pocopine_host)]
static NEXT_POST_ID: AtomicU64 = AtomicU64::new(3);
#[cfg(pocopine_host)]
static LIVE_BACKEND: OnceLock<MemoryEventBackend> = OnceLock::new();

#[cfg(pocopine_host)]
fn posts() -> &'static Mutex<Vec<Post>> {
    POSTS.get_or_init(|| {
        Mutex::new(vec![
            Post {
                id: "post_1".to_string(),
                title: "Seeded post".to_string(),
                body: "This row came from the process-local store.".to_string(),
                version: 1,
            },
            Post {
                id: "post_2".to_string(),
                title: "Live updates".to_string(),
                body: "Create a post in another tab and this list refreshes.".to_string(),
                version: 1,
            },
        ])
    })
}

#[cfg(pocopine_host)]
pub fn live_backend() -> MemoryEventBackend {
    LIVE_BACKEND.get_or_init(MemoryEventBackend::new).clone()
}

#[cfg(pocopine_host)]
async fn publish_posts_invalidation(op: pocopine::live::LiveOp, key: impl Into<String>) {
    let collection_draft = pocopine::live::LiveInvalidation::new("posts", op)
        .keys([key.into()])
        .query_tags(["posts:list"])
        .into_draft();

    publish_live_draft(collection_draft).await;

    let query_draft = pocopine::live::query_tag_topic("posts:list")
        .and_then(|topic| pocopine::live::query_invalidated(topic, ["posts:list"]));

    publish_live_draft(query_draft).await;
}

#[cfg(pocopine_host)]
async fn publish_live_draft(draft: pocopine_events::EventResult<pocopine_events::EventDraft>) {
    let Ok(draft) = draft else {
        return;
    };

    if let Err(err) = live_backend().publish(draft).await {
        tracing::warn!(
            target: "pocopine.log",
            error = %err,
            "failed to publish live posts invalidation"
        );
    }
}

#[pocopine::server(public)]
pub async fn list_posts() -> ServerResult<Vec<Post>> {
    let mut posts = posts()
        .lock()
        .map_err(|_| ServerError::App("posts store lock poisoned".to_string()))?
        .clone();
    posts.sort_by(|a, b| b.version.cmp(&a.version).then_with(|| b.id.cmp(&a.id)));
    Ok(posts)
}

#[pocopine::server(public)]
pub async fn create_post(draft: PostDraft) -> ServerResult<Post> {
    let title = draft.title.trim();
    let body = draft.body.trim();
    if title.is_empty() || body.is_empty() {
        return Err(ServerError::App("title and body are required".to_string()));
    }

    let next_id = NEXT_POST_ID.fetch_add(1, Ordering::Relaxed);
    let post = Post {
        id: format!("post_{next_id}"),
        title: title.to_string(),
        body: body.to_string(),
        version: next_id,
    };

    posts()
        .lock()
        .map_err(|_| ServerError::App("posts store lock poisoned".to_string()))?
        .push(post.clone());
    publish_posts_invalidation(pocopine::live::LiveOp::Upsert, post.id.clone()).await;
    Ok(post)
}

#[pocopine::server(public)]
pub async fn reset_posts() -> ServerResult<()> {
    posts()
        .lock()
        .map_err(|_| ServerError::App("posts store lock poisoned".to_string()))?
        .clear();
    publish_posts_invalidation(pocopine::live::LiveOp::Reset, "posts").await;
    Ok(())
}

#[derive(Default, Serialize, Deserialize)]
#[component(style = "live_board.css")]
pub struct LiveBoard {
    pub title: String,
    pub body: String,
    pub posts: Vec<Post>,
    pub loading: bool,
    pub saving: bool,
    pub error: String,
    pub status: String,
    pub event_count: u32,
}

#[handlers]
impl LiveBoard {
    pub fn on_mount(&mut self) {
        self.status = "live stream opening".to_string();
        self.reload();

        #[cfg(pocopine_browser)]
        {
            let handle = this::<Self>();
            let refresh_handle = handle.clone();
            let gap_handle = handle.clone();
            let error_handle = handle;

            if let Err(err) = pocopine::live::LiveRefresh::scoped()
                .query_tag("posts:list", move |_event| {
                    refresh_handle.update(|s| {
                        s.event_count += 1;
                        s.status = "live event received".to_string();
                        s.reload();
                    });
                })
                .on_gap(move |_event| {
                    gap_handle.update(|s| {
                        s.status = "live cursor gap; refetching".to_string();
                    });
                })
                .on_error(move |event| {
                    error_handle.update(|s| {
                        s.status = "live stream error".to_string();
                        s.error = format!("{:?}", event.live_event);
                    });
                })
                .open()
            {
                self.status = "live stream failed".to_string();
                self.error = err.to_string();
            }
        }
    }

    pub fn create(&mut self) {
        if self.title.trim().is_empty() || self.body.trim().is_empty() {
            self.error = "title and body are required".to_string();
            return;
        }

        self.saving = true;
        self.error.clear();
        self.status = "saving".to_string();
        let draft = PostDraft {
            title: self.title.clone(),
            body: self.body.clone(),
        };

        dispatch!(create_post(draft).await, |s, result| {
            s.saving = false;
            match result {
                Ok(_) => {
                    s.title.clear();
                    s.body.clear();
                    s.status = "saved; waiting for live refresh".to_string();
                }
                Err(err) => {
                    s.status = "save failed".to_string();
                    s.error = err.to_string();
                }
            }
        },);
    }

    pub fn reload(&mut self) {
        self.loading = true;
        dispatch!(list_posts().await, |s, result| {
            s.loading = false;
            match result {
                Ok(posts) => {
                    s.posts = posts;
                    s.error.clear();
                    if s.status.is_empty() || s.status == "live stream opening" {
                        s.status = "ready".to_string();
                    }
                }
                Err(err) => {
                    s.status = "refresh failed".to_string();
                    s.error = err.to_string();
                }
            }
        },);
    }

    pub fn reset(&mut self) {
        self.saving = true;
        self.error.clear();
        self.status = "resetting".to_string();
        dispatch!(reset_posts().await, |s, result| {
            s.saving = false;
            match result {
                Ok(()) => {
                    s.status = "reset; waiting for live refresh".to_string();
                }
                Err(err) => {
                    s.status = "reset failed".to_string();
                    s.error = err.to_string();
                }
            }
        },);
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<LiveBoard>().run();
}
