//! Blog example — fetches a single post from a `#[server]` function and
//! renders it. Demonstrates:
//!
//! * tag-based composition (`<blog-post post-id="1">` from index.html),
//! * the `dispatch!` macro for async state updates,
//! * native-Rust field mutations (no `JsValue`, no `Reflect::set`,
//!   no scope-id plumbing).

pub mod shared;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use shared::Post;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PostViewAudit {
    pub post_id: u32,
}

#[pocopine::job(queue = "blog", retries = 2)]
pub async fn record_post_view(input: PostViewAudit) -> JobResult<()> {
    tracing::info!(
        target: "pocopine.log",
        post_id = input.post_id,
        "post view audit"
    );
    Ok(())
}

#[pocopine::job(queue = "blog", every = "10m")]
pub async fn refresh_blog_index() -> JobResult<()> {
    tracing::info!(target: "pocopine.log", "refresh blog index");
    Ok(())
}

/// `#[server]` generates:
/// * on wasm32 — a client stub that POSTs `[post_id]` to the
///   generated route and deserializes the response.
/// * on the host — this body is the real handler, and the route is
///   registered automatically by `pocopine_server::Server`.
#[pocopine::server(public)]
pub async fn get_post(post_id: u32) -> ServerResult<Post> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = record_post_view_job::enqueue(PostViewAudit { post_id }).await;
    }

    match post_id {
        1 => Ok(Post {
            id: 1,
            title: "Hello from pocopine".into(),
            body: "This body was fetched from a #[server] function over \
                   a typed REST binding, written once in Rust and shared \
                   across wasm + host builds."
                .into(),
        }),
        _ => Err(pocopine::ServerError::App(format!(
            "no post with id {post_id}"
        ))),
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct BlogPost {
    #[prop]
    pub post_id: u32,
    pub title: String,
    pub body: String,
    pub loading: bool,
    pub error: String,
}

#[handlers]
impl BlogPost {
    pub fn on_mount(&mut self) {
        self.loading = true;
        let post_id = self.post_id;
        dispatch!(get_post(post_id).await, |s, result| {
            s.loading = false;
            match result {
                Ok(p) => {
                    s.title = p.title;
                    s.body = p.body;
                    s.error.clear();
                }
                Err(e) => {
                    s.error = e.to_string();
                }
            }
        },);
    }

    pub fn refresh(&mut self) {
        // Re-run the same load path.
        self.on_mount();
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<BlogPost>().run();
}
