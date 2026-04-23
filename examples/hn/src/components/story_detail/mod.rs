//! Single story + its comment tree. Path param `:id` → `pub id: u32`
//! via the router's kebab→snake prop coercion.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::Comment;
use crate::shared::ItemNode;
use crate::{extract_domain, get_item_tree, humanize_age, performance_now};

#[derive(Default, Serialize, Deserialize)]
#[component(style = "story_detail.css")]
pub struct StoryDetail {
    #[prop]
    pub id: u32,
    pub title: String,
    pub author: String,
    pub points: i32,
    pub url: String,
    pub domain: String,
    pub age: String,
    pub text: String,
    pub comments: Vec<Comment>,
    pub comment_count: u32,
    pub loading: bool,
    pub error: String,
    pub fetch_ms: u32,
}

#[handlers]
impl StoryDetail {
    pub fn on_mount(&mut self) {
        self.loading = true;
        let id = self.id;
        let start = performance_now();
        dispatch!(get_item_tree(id).await, |s, result| {
            s.loading = false;
            s.fetch_ms = (performance_now() - start).round().max(0.0) as u32;
            match result {
                Ok(item) => {
                    s.title = item.title.clone().unwrap_or_default();
                    s.author = item.author.clone().unwrap_or_default();
                    s.points = item.points.unwrap_or(0);
                    let url = item.url.clone().unwrap_or_default();
                    s.domain = extract_domain(&url);
                    s.url = url;
                    s.age = humanize_age(item.created_at_i);
                    s.text = item.text.clone().unwrap_or_default();
                    s.comments = build_comments(&item.children);
                    s.comment_count = count_comments(&s.comments) as u32;
                    s.error.clear();
                }
                Err(e) => s.error = e.to_string(),
            }
        },);
    }
}

/// Convert wire-shape `ItemNode` (HN Algolia) to the client-side
/// `Comment` display shape. Filters out dead / deleted nodes and
/// pre-formats the age so the template stays declarative.
fn build_comments(nodes: &[ItemNode]) -> Vec<Comment> {
    nodes
        .iter()
        .filter(|n| n.author.is_some() && n.text.is_some())
        .map(|n| Comment {
            id: n.id,
            author: n.author.clone().unwrap_or_default(),
            age: humanize_age(n.created_at_i),
            body: n.text.clone().unwrap_or_default(),
            children: build_comments(&n.children),
        })
        .collect()
}

fn count_comments(nodes: &[Comment]) -> usize {
    nodes.iter().map(|c| 1 + count_comments(&c.children)).sum()
}
