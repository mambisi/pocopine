use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

mod data;

pub use data::query::{create_post, list_posts, reset_posts, Post, PostDraft};

#[cfg(pocopine_host)]
pub use data::query::{__create_post_route, __list_posts_route, __reset_posts_route, live_backend};

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
