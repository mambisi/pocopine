//! pocopine HN — a Hacker News frontend.
//!
//! Two server functions talk to the HN Algolia search API:
//!
//! * `search_stories(query)` — empty query returns the front page;
//!   otherwise searches story titles / text.
//! * `get_item_tree(id)`     — story + full nested comment tree.
//!
//! The client renders the results through `dispatch!` + `pp-html`.

pub mod shared;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use shared::{ItemNode, Story};
#[cfg(not(target_arch = "wasm32"))]
use shared::SearchResult;

// ─── server-side helpers ────────────────────────────────────────

#[cfg(not(target_arch = "wasm32"))]
const ALGOLIA: &str = "https://hn.algolia.com/api/v1";

#[cfg(not(target_arch = "wasm32"))]
async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, pocopine::ServerError> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| pocopine::ServerError::App(format!("fetch {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(pocopine::ServerError::App(format!(
            "{url} returned HTTP {}",
            resp.status()
        )));
    }
    resp.json::<T>()
        .await
        .map_err(|e| pocopine::ServerError::App(format!("decode {url}: {e}")))
}

// ─── server functions ──────────────────────────────────────────

/// Search story titles / text. An empty query returns the front page
/// (the same 30 stories news.ycombinator.com shows). Otherwise
/// searches `tags=story` via the Algolia index, ordered by relevance.
#[pocopine::server]
pub async fn search_stories(query: String) -> ServerResult<Vec<Story>> {
    let trimmed = query.trim();
    let url = if trimmed.is_empty() {
        format!("{ALGOLIA}/search?tags=front_page&hitsPerPage=30")
    } else {
        let encoded = percent_encode(trimmed);
        format!("{ALGOLIA}/search?query={encoded}&tags=story&hitsPerPage=30")
    };
    let result: SearchResult = fetch_json(&url).await?;
    Ok(result.hits)
}

/// Story + full comment subtree. Algolia pre-populates `children`, so
/// there is no N+1 fan-out — one request, the whole thread.
#[pocopine::server]
pub async fn get_item_tree(id: u32) -> ServerResult<ItemNode> {
    fetch_json(&format!("{ALGOLIA}/items/{id}")).await
}

/// Minimal `application/x-www-form-urlencoded` style percent-encoder —
/// enough for our use (small, ASCII-ish query strings). Avoids adding
/// a dep just for URL encoding.
#[cfg(not(target_arch = "wasm32"))]
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ─── components ────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct AppShell {}

#[handlers]
impl AppShell {}

/// Row shape the StoryList template iterates. All display-time
/// derivations live here so the `pp-for` body stays declarative.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct StoryView {
    pub id: String,
    pub title: String,
    pub title_href: String,
    pub external: bool,
    pub domain: String,
    pub points: i32,
    pub author: String,
    pub age: String,
    pub item_url: String,
    pub comments_label: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct StoryList {
    pub query: String,
    pub applied_query: String,
    pub loading: bool,
    pub error: String,
    pub stories: Vec<StoryView>,
    pub count: u32,
    pub fetch_ms: u32,
}

#[handlers]
impl StoryList {
    pub fn init(&mut self) {
        if self.count > 0 {
            return;
        }
        self.run_search();
    }

    /// Triggered by the search form (pp-on:submit.prevent) or any
    /// button that wants to re-run the current query.
    pub fn search(&mut self) {
        self.run_search();
    }

    fn run_search(&mut self) {
        self.loading = true;
        let query = self.query.clone();
        let start = performance_now();
        dispatch!(search_stories(query.clone()).await, |s, result| {
            s.loading = false;
            s.applied_query = query.clone();
            s.fetch_ms = (performance_now() - start).round().max(0.0) as u32;
            match result {
                Ok(stories) => {
                    s.count = stories.len() as u32;
                    s.stories = stories.into_iter().map(story_to_view).collect();
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
    pub fn init(&mut self) {
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

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}

// ─── rendering helpers ─────────────────────────────────────────

fn story_to_view(s: Story) -> StoryView {
    let url = s.url.clone().unwrap_or_default();
    let external = !url.is_empty();
    let domain = extract_domain(&url);
    let item_url = format!("/item/{}", s.id);
    let title_href = if external { url } else { item_url.clone() };
    let comments_label = match s.num_comments {
        None | Some(0) => "discuss".to_string(),
        Some(1) => "1 comment".to_string(),
        Some(n) => format!("{n} comments"),
    };
    StoryView {
        id: s.id,
        title: s.title,
        title_href,
        external,
        domain,
        points: s.points.unwrap_or(0),
        author: s.author,
        age: humanize_age(s.created_at_i),
        item_url,
        comments_label,
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

fn extract_domain(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = after_scheme.split('/').next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// "5 minutes ago" / "2 hours ago" / "3 days ago".
fn humanize_age(created_at_i: i64) -> String {
    if created_at_i <= 0 {
        return String::new();
    }
    let now = now_seconds();
    let delta = now.saturating_sub(created_at_i);
    if delta < 60 {
        return "just now".into();
    }
    let (n, unit) = if delta < 3600 {
        (delta / 60, "minute")
    } else if delta < 86400 {
        (delta / 3600, "hour")
    } else if delta < 2_592_000 {
        (delta / 86400, "day")
    } else if delta < 31_536_000 {
        (delta / 2_592_000, "month")
    } else {
        (delta / 31_536_000, "year")
    };
    let suffix = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{suffix} ago")
}

#[cfg(target_arch = "wasm32")]
fn now_seconds() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

#[cfg(not(target_arch = "wasm32"))]
fn now_seconds() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
