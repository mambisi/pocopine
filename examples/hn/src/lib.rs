//! pocopine HN — a Hacker News frontend.
//!
//! Two server functions talk to the HN Algolia search API:
//!
//! * `search_stories(query)` — empty query returns the front page;
//!   otherwise searches story titles / text.
//! * `get_item_tree(id)`     — story + full nested comment tree.
//!
//! Components live under [`components`]; the client entry and router
//! wiring live in [`app`].

pub mod app;
pub mod components;
pub mod shared;

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
pub async fn search_stories(query: String) -> pocopine::ServerResult<Vec<Story>> {
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
pub async fn get_item_tree(id: u32) -> pocopine::ServerResult<ItemNode> {
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

// ─── formatting / parsing helpers shared across components ─────

pub fn extract_domain(url: &str) -> String {
    if url.is_empty() {
        return String::new();
    }
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = after_scheme.split('/').next().unwrap_or("");
    host.strip_prefix("www.").unwrap_or(host).to_string()
}

/// "5 minutes ago" / "2 hours ago" / "3 days ago".
pub fn humanize_age(created_at_i: i64) -> String {
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

pub fn performance_now() -> f64 {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.performance())
            .map(|p| p.now())
            .unwrap_or(0.0)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0.0
    }
}
