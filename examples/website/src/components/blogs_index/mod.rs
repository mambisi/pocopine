//! `/blogs` — the blog index. Lists every post from the build-time
//! `BLOGS` index (front-matter title / description / date, newest
//! first), linking each card to `/blogs/<slug>`. Data-driven: dropping
//! a new `docs/blogs/*.md` adds it here with no code change.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::docs_data;

/// One post card on the index.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct PostRow {
    pub title: String,
    pub description: String,
    pub date: String,
    pub href: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "BlogsIndex.poco",
    style = "blogs_index.css",
    role = "panel"
)]
pub struct BlogsIndex {
    pub posts: Vec<PostRow>,
}

#[handlers]
impl BlogsIndex {
    // on_setup (pre-render) so the `pp-for` has data when the compiled
    // template installs clone directives.
    pub fn on_setup(&mut self) {
        self.posts = docs_data::BLOGS
            .iter()
            .map(|b| PostRow {
                title: b.title.into(),
                description: b.description.into(),
                date: b.date_display.into(),
                href: format!("/blogs/{}", b.slug),
            })
            .collect();
    }

    /// Delegate clicks on the post links — `pp-route` cannot be used
    /// inside a `pp-for` clone (see `delegate_nav`).
    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        crate::components::delegate_nav(&ev);
    }
}

impl RouteComponent for BlogsIndex {}
