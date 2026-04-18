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

/// `#[server]` generates:
/// * on wasm32 — a client stub that POSTs `[post_id]` to
///   `/_pocopine/get_post` and deserializes the response.
/// * on the host — this body is the real handler, plus a
///   `__get_post_route` helper used by `bin/server.rs`.
#[pocopine::server]
pub async fn get_post(post_id: u32) -> ServerResult<Post> {
    match post_id {
        1 => Ok(Post {
            id: 1,
            title: "Hello from pocopine".into(),
            body: "This body was fetched from a #[server] function over \
                   a typed REST binding, written once in Rust and shared \
                   across wasm + host builds.".into(),
        }),
        _ => Err(pocopine::ServerError::App(format!(
            "no post with id {post_id}"
        ))),
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct BlogPost {
    pub post_id: u32,
    pub title: String,
    pub body: String,
    pub loading: bool,
    pub error: String,
}

#[handlers]
impl BlogPost {
    pub fn init(&mut self) {
        self.loading = true;
        let post_id = self.post_id;
        dispatch!(
            get_post(post_id).await,
            |s, result| {
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
            },
        );
    }

    pub fn refresh(&mut self) {
        // Re-run the same load path.
        self.init();
    }
}

#[wasm_bindgen(start)]
pub fn main() {
    App::new().register::<BlogPost>().run();
}
