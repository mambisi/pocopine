//! pocopine HN — a Hacker News frontend.
//!
//! Server functions call the public HN Firebase API
//! (`hacker-news.firebaseio.com`). The client renders story lists and
//! comment threads via the same `dispatch!` + `pp-html` pattern used
//! in every other pocopine example — no custom plumbing.

pub mod shared;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use shared::Item;

// ─── server-side helpers (host-only) ────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
const HN_API: &str = "https://hacker-news.firebaseio.com/v0";

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, pocopine::ServerError> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| pocopine::ServerError::App(format!("fetch {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(pocopine::ServerError::App(format!(
            "HN API {} returned {}",
            url,
            resp.status()
        )));
    }
    resp.json::<T>()
        .await
        .map_err(|e| pocopine::ServerError::App(format!("decode {url}: {e}")))
}

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_item(id: u32) -> Result<Item, pocopine::ServerError> {
    fetch_json::<Item>(&format!("{HN_API}/item/{id}.json")).await
}

// ─── server functions ──────────────────────────────────────────

/// Top-of-page story feed. `limit` is clamped to 50 so a misbehaving
/// client can't ask the server to fan out to hundreds of HN calls.
#[pocopine::server]
pub async fn top_stories(limit: u32) -> ServerResult<Vec<Item>> {
    let ids: Vec<u32> = fetch_json(&format!("{HN_API}/topstories.json")).await?;
    let take = (limit as usize).min(ids.len()).min(50);
    let futs = ids[..take].iter().copied().map(fetch_item);
    let results = futures::future::join_all(futs).await;
    Ok(results.into_iter().filter_map(Result::ok).collect())
}

/// Fetch a single item (story, comment, …) by id.
#[pocopine::server]
pub async fn get_item(id: u32) -> ServerResult<Item> {
    fetch_item(id).await
}

/// Fetch the top-level comments for a story. Returns up to 30 of the
/// story's direct `kids`. Nested replies aren't followed (a follow-up
/// would be a recursive `get_comment_tree`).
#[pocopine::server]
pub async fn get_comments(parent_id: u32) -> ServerResult<Vec<Item>> {
    let parent = fetch_item(parent_id).await?;
    let kids: Vec<u32> = parent.kids.iter().copied().take(30).collect();
    let futs = kids.into_iter().map(fetch_item);
    let results = futures::future::join_all(futs).await;
    Ok(results.into_iter().filter_map(Result::ok).collect())
}

// ─── components ────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct AppShell {}

#[handlers]
impl AppShell {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct StoryList {
    pub loading: bool,
    pub error: String,
    pub list_html: String,
    pub count: u32,
    pub fetch_ms: u32,
}

#[handlers]
impl StoryList {
    pub fn init(&mut self) {
        if self.count > 0 {
            return; // already loaded
        }
        self.loading = true;
        let start = performance_now();
        dispatch!(top_stories(30).await, |s, result| {
            s.loading = false;
            s.fetch_ms = (performance_now() - start).round().max(0.0) as u32;
            match result {
                Ok(stories) => {
                    s.count = stories.len() as u32;
                    s.list_html = render_story_list(&stories);
                    s.error.clear();
                }
                Err(e) => s.error = e.to_string(),
            }
        },);
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct StoryDetail {
    pub id: u32,
    pub title: String,
    pub author: String,
    pub score: i32,
    pub descendants: u32,
    pub url: String,
    pub domain: String,
    pub text: String,
    pub loading: bool,
    pub error: String,
    pub comments_html: String,
    pub comments_loading: bool,
    pub comments_error: String,
}

#[handlers]
impl StoryDetail {
    pub fn init(&mut self) {
        self.loading = true;
        let id = self.id;
        dispatch!(get_item(id).await, |s, result| {
            s.loading = false;
            match result {
                Ok(item) => {
                    s.title = item.title;
                    s.author = item.by;
                    s.score = item.score;
                    s.descendants = item.descendants;
                    s.domain = extract_domain(&item.url);
                    s.url = item.url;
                    s.text = item.text;
                    s.error.clear();
                    s.load_comments();
                }
                Err(e) => s.error = e.to_string(),
            }
        },);
    }

    pub fn load_comments(&mut self) {
        self.comments_loading = true;
        let id = self.id;
        dispatch!(get_comments(id).await, |s, result| {
            s.comments_loading = false;
            match result {
                Ok(comments) => {
                    s.comments_html = render_comments(&comments);
                    s.comments_error.clear();
                }
                Err(e) => s.comments_error = e.to_string(),
            }
        },);
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}

// ─── rendering helpers ─────────────────────────────────────────

fn render_story_list(stories: &[Item]) -> String {
    let mut out = String::from("<ol class=\"stories\" start=\"1\">");
    for s in stories {
        let domain = extract_domain(&s.url);
        let target = if s.url.is_empty() {
            format!("/item/{}", s.id)
        } else {
            s.url.clone()
        };
        let external = !s.url.is_empty();
        let external_attrs = if external {
            " target=\"_blank\" rel=\"noopener noreferrer\""
        } else {
            " pp-route"
        };
        out.push_str(&format!(
            "<li class=\"story\">\
                <div class=\"story__head\">\
                    <a class=\"story__title\" href=\"{target}\"{external_attrs}>{title}</a>\
                    {domain_html}\
                </div>\
                <div class=\"story__meta\">\
                    <span class=\"score\">{score}</span> points\
                    &middot; by {author}\
                    &middot; \
                    <a href=\"/item/{id}\" pp-route>{descendants} comments</a>\
                </div>\
            </li>",
            target = html_escape(&target),
            external_attrs = external_attrs,
            title = html_escape(&s.title),
            domain_html = if domain.is_empty() {
                String::new()
            } else {
                format!(" <span class=\"domain\">({})</span>", html_escape(&domain))
            },
            score = s.score,
            author = html_escape(&s.by),
            id = s.id,
            descendants = s.descendants,
        ));
    }
    out.push_str("</ol>");
    out
}

fn render_comments(comments: &[Item]) -> String {
    let live: Vec<&Item> = comments.iter().filter(|c| !c.deleted && !c.dead).collect();
    if live.is_empty() {
        return "<p class=\"status\"><em>No comments yet.</em></p>".into();
    }
    let mut out = String::from("<ul class=\"comments\">");
    for c in live {
        out.push_str(&format!(
            "<li class=\"comment\">\
                <div class=\"comment__meta\">by {author}</div>\
                <div class=\"comment__body\">{body}</div>\
            </li>",
            author = html_escape(&c.by),
            // HN comment text is already HTML from the API — render as-is.
            body = c.text,
        ));
    }
    out.push_str("</ul>");
    out
}

fn extract_domain(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = after_scheme.split('/').next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            _ => c.to_string(),
        })
        .collect()
}

fn performance_now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

// ─── entry ────────────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .route::<StoryList>("/")
        .route::<StoryDetail>("/item/:id")
        .route::<NotFound>("*")
        .run();
}
