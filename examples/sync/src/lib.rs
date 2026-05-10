use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[cfg(pocopine_host)]
use {
    pocopine_events::MemoryEventBackend,
    pocopine_sync::{MemorySyncStream, SyncServer},
    std::sync::{Arc, OnceLock},
};

pub const POSTS_STREAM: &str = "posts_for_user";
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
    pub next_local_id: u64,
}

#[cfg(pocopine_host)]
static POSTS_SYNC: OnceLock<MemorySyncStream<Post>> = OnceLock::new();
#[cfg(pocopine_host)]
static LIVE_BACKEND: OnceLock<MemoryEventBackend> = OnceLock::new();
#[cfg(pocopine_host)]
static SYNC_SERVER: OnceLock<SyncServer> = OnceLock::new();

#[cfg(pocopine_host)]
pub fn posts_stream() -> MemorySyncStream<Post> {
    POSTS_SYNC
        .get_or_init(|| {
            let stream = MemorySyncStream::new(POSTS_STREAM, POSTS_COLLECTION)
                .expect("sync example stream names must be valid");
            stream
                .upsert(
                    "post_1",
                    Post {
                        id: "post_1".to_string(),
                        title: "Seeded sync row".to_string(),
                        body: "This row was loaded through the sync pull endpoint.".to_string(),
                    },
                )
                .expect("seed post should sync");
            stream
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
            stream
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
                .public_stream(posts_stream())
                .events(Arc::new(live_backend()))
                .build()
        })
        .clone()
}

#[cfg(pocopine_host)]
async fn invalidate_posts() {
    if let Err(err) = sync_server().invalidate_stream(POSTS_STREAM).await {
        tracing::warn!(
            target: "pocopine.log",
            error = %err,
            "failed to publish sync posts invalidation"
        );
    }
}

#[pocopine::server(public)]
pub async fn reset_posts() -> ServerResult<()> {
    posts_stream()
        .reset()
        .map_err(|err| ServerError::App(err.to_string()))?;
    invalidate_posts().await;
    Ok(())
}

#[handlers]
impl SyncBoard {
    pub fn on_mount(&mut self) {
        self.status = "opening sync stream".to_string();
        let result = self
            .plugin::<pocopine_sync::SyncClient>()
            .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.posts)
            .stream(POSTS_STREAM)
            .and_then(|collection| collection.open());

        if let Err(err) = result {
            self.status = "sync failed".to_string();
            self.posts.set_error(err.to_string());
        }
    }

    pub fn create(&mut self) {
        let title = self.title.trim().to_string();
        let body = self.body.trim().to_string();
        if title.is_empty() || body.is_empty() {
            self.posts.set_error("title and body are required");
            return;
        }

        self.posts.clear_error();
        self.next_local_id = self.next_local_id.saturating_add(1);
        let post = Post {
            id: format!("post_local_{}", self.next_local_id),
            title,
            body,
        };

        let result = build_create_mutation(post.clone()).and_then(|(mutation, optimistic)| {
            self.plugin::<pocopine_sync::SyncClient>()
                .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.posts)
                .stream(POSTS_STREAM)
                .and_then(|collection| collection.push(mutation, Some(optimistic)))
        });

        match result {
            Ok(()) => {
                self.title.clear();
                self.body.clear();
                self.status = "ready".to_string();
            }
            Err(err) => {
                self.status = "push failed".to_string();
                self.posts.set_error(err.to_string());
            }
        }
    }

    pub fn refresh(&mut self) {
        self.status = "pulling changes".to_string();
        let result = self
            .plugin::<pocopine_sync::SyncClient>()
            .collection(pocopine::this::<Self>(), |s: &mut Self| &mut s.posts)
            .stream(POSTS_STREAM)
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

fn build_create_mutation(
    post: Post,
) -> pocopine_sync::SyncResult<(
    pocopine_sync::ClientMutation<Post>,
    pocopine_sync::SyncRow<Post>,
)> {
    let key = pocopine_sync::RowKey::new(post.id.clone())?;
    let mutation = pocopine_sync::ClientMutation {
        id: pocopine_sync::MutationId::new(format!("create_post:{}", post.id))?,
        key: Some(key),
        op: pocopine_sync::SyncOp::Upsert,
        base_version: None,
        payload: post.clone(),
    };
    let optimistic = pocopine_sync::SyncRow::new(post.id.clone(), post)?;
    Ok((mutation, optimistic))
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .plugin(pocopine_sync::sync_plugin().with_live_wakeup(true))
        .register::<SyncBoard>()
        .run();
}
