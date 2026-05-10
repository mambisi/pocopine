use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(pocopine_host)]
use {
    pocopine_events::MemoryEventBackend,
    pocopine_sync::{MemorySyncShape, SyncServer},
    std::sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
};

pub const POSTS_SHAPE: &str = "posts_for_user";
pub const POSTS_COLLECTION: &str = "posts";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Post {
    pub id: String,
    pub title: String,
    pub body: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(style = "sync_board.css")]
pub struct SyncBoard {
    pub title: String,
    pub body: String,
    pub posts: pocopine_sync::CollectionState<Post>,
    pub saving: bool,
    pub status: String,
}

#[cfg(pocopine_host)]
static POSTS_SYNC: OnceLock<MemorySyncShape<Post>> = OnceLock::new();
#[cfg(pocopine_host)]
static LIVE_BACKEND: OnceLock<MemoryEventBackend> = OnceLock::new();
#[cfg(pocopine_host)]
static SYNC_SERVER: OnceLock<SyncServer> = OnceLock::new();
#[cfg(pocopine_host)]
static NEXT_POST_ID: AtomicU64 = AtomicU64::new(3);

#[cfg(pocopine_host)]
pub fn posts_shape() -> MemorySyncShape<Post> {
    POSTS_SYNC
        .get_or_init(|| {
            let shape = MemorySyncShape::new(POSTS_SHAPE, POSTS_COLLECTION)
                .expect("sync example shape names must be valid");
            shape
                .upsert(
                    "post_1",
                    Post {
                        id: "post_1".to_string(),
                        title: "Seeded sync row".to_string(),
                        body: "This row was loaded through the sync pull endpoint.".to_string(),
                    },
                )
                .expect("seed post should sync");
            shape
                .upsert(
                    "post_2",
                    Post {
                        id: "post_2".to_string(),
                        title: "Live wake-up".to_string(),
                        body: "Create a post in another tab; SSE wakes the client, then sync pulls the change."
                            .to_string(),
                    },
                )
                .expect("seed post should sync");
            shape
        })
        .clone()
}

#[cfg(pocopine_host)]
pub fn live_backend() -> MemoryEventBackend {
    LIVE_BACKEND.get_or_init(MemoryEventBackend::new).clone()
}

#[cfg(pocopine_host)]
pub fn sync_server() -> SyncServer {
    SYNC_SERVER
        .get_or_init(|| {
            SyncServer::builder()
                .shape(posts_shape())
                .events(Arc::new(live_backend()))
                .build()
        })
        .clone()
}

#[cfg(pocopine_host)]
async fn invalidate_posts() {
    if let Err(err) = sync_server().invalidate_shape(POSTS_SHAPE).await {
        tracing::warn!(
            target: "pocopine.log",
            error = %err,
            "failed to publish sync posts invalidation"
        );
    }
}

#[pocopine::server(public)]
pub async fn create_post(title: String, body: String) -> ServerResult<Post> {
    let title = title.trim();
    let body = body.trim();
    if title.is_empty() || body.is_empty() {
        return Err(ServerError::BadRequest(
            "title and body are required".to_string(),
        ));
    }

    let next_id = NEXT_POST_ID.fetch_add(1, Ordering::Relaxed);
    let post = Post {
        id: format!("post_{next_id}"),
        title: title.to_string(),
        body: body.to_string(),
    };

    posts_shape()
        .upsert(post.id.clone(), post.clone())
        .map_err(|err| ServerError::App(err.to_string()))?;
    invalidate_posts().await;
    Ok(post)
}

#[pocopine::server(public)]
pub async fn reset_posts() -> ServerResult<()> {
    posts_shape()
        .reset()
        .map_err(|err| ServerError::App(err.to_string()))?;
    invalidate_posts().await;
    Ok(())
}

#[handlers]
impl SyncBoard {
    pub fn on_mount(&mut self) {
        self.status = "opening sync shape".to_string();
        let result = self
            .plugin::<pocopine_sync::SyncClient>()
            .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.posts)
            .shape(POSTS_SHAPE)
            .and_then(|collection| collection.open());

        if let Err(err) = result {
            self.status = "sync failed".to_string();
            self.posts.set_error(err.to_string());
        }
    }

    pub fn create(&mut self) {
        if self.title.trim().is_empty() || self.body.trim().is_empty() {
            self.posts.set_error("title and body are required");
            return;
        }

        self.saving = true;
        self.posts.clear_error();
        self.status = "saving".to_string();
        let title = self.title.clone();
        let body = self.body.clone();

        dispatch!(create_post(title, body).await, |s, result| {
            s.saving = false;
            match result {
                Ok(_) => {
                    s.title.clear();
                    s.body.clear();
                    s.status = "saved".to_string();
                }
                Err(err) => {
                    s.status = "save failed".to_string();
                    s.posts.set_error(err.to_string());
                }
            }
        });
    }

    pub fn refresh(&mut self) {
        self.status = "pulling changes".to_string();
        let result = self
            .plugin::<pocopine_sync::SyncClient>()
            .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.posts)
            .shape(POSTS_SHAPE)
            .and_then(|collection| collection.pull());

        if let Err(err) = result {
            self.status = "refresh failed".to_string();
            self.posts.set_error(err.to_string());
        }
    }

    pub fn reset(&mut self) {
        self.saving = true;
        self.posts.clear_error();
        self.status = "resetting".to_string();
        dispatch!(reset_posts().await, |s, result| {
            s.saving = false;
            match result {
                Ok(()) => {
                    s.status = "reset".to_string();
                }
                Err(err) => {
                    s.status = "reset failed".to_string();
                    s.posts.set_error(err.to_string());
                }
            }
        });
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .plugin(pocopine_sync::sync_plugin().with_live_wakeup(true))
        .register::<SyncBoard>()
        .run();
}
