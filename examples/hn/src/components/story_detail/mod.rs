//! Single story + its comment tree. Path param `:id` → `pub id: u32`
//! via the router's kebab→snake prop coercion.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::shared::ItemNode;
use crate::{extract_domain, get_item_tree, html_escape, humanize_age, performance_now};

#[derive(Default, Serialize, Deserialize)]
#[component(style = "story_detail.css")]
pub struct StoryDetail {
    pub id: u32,
    pub title: String,
    pub author: String,
    pub points: i32,
    pub url: String,
    pub domain: String,
    pub age: String,
    pub text: String,
    pub comments_html: String,
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
                    s.comment_count = count_comments(&item) as u32;
                    s.comments_html = render_comment_tree(&item.children, 0);
                    s.error.clear();
                }
                Err(e) => s.error = e.to_string(),
            }
        },);
    }
}

fn render_comment_tree(nodes: &[ItemNode], depth: usize) -> String {
    let live: Vec<&ItemNode> = nodes
        .iter()
        .filter(|n| n.author.is_some() && n.text.is_some())
        .collect();
    if live.is_empty() {
        return String::new();
    }
    let depth_class = if depth == 0 { "" } else { " comments--nested" };
    let mut out = format!("<ul class=\"comments{depth_class}\">");
    for c in live {
        let author = c.author.clone().unwrap_or_default();
        let age = humanize_age(c.created_at_i);
        out.push_str(&format!(
            "<li class=\"comment\">\
                <div class=\"comment__meta\">\
                    <span class=\"comment__author\">{author}</span>\
                    <span class=\"sep\">·</span>\
                    <span class=\"age\">{age}</span>\
                </div>\
                <div class=\"comment__body\">{body}</div>\
                {children}\
            </li>",
            author = html_escape(&author),
            age = html_escape(&age),
            body = c.text.clone().unwrap_or_default(),
            children = render_comment_tree(&c.children, depth + 1),
        ));
    }
    out.push_str("</ul>");
    out
}

fn count_comments(node: &ItemNode) -> usize {
    node.children.iter().map(|c| 1 + count_comments(c)).sum()
}
