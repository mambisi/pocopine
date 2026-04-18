//! pocopine — the marketing site. Client-side SPA + server functions.

pub mod shared;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use shared::{Article, ArticleSummary, ContactMessage, ContactResponse};
#[cfg(not(target_arch = "wasm32"))]
use shared::human_bytes;

// ─── server functions ────────────────────────────────────────────

/// Host-side helper — reads articles.json off disk. Shared by the two
/// server fns below so they parse once. Target-gated because the
/// client stub never executes this body.
#[cfg(not(target_arch = "wasm32"))]
fn load_articles_from_disk() -> Result<Vec<Article>, pocopine::ServerError> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/articles.json");
    let text = std::fs::read_to_string(path)
        .map_err(|e| pocopine::ServerError::App(format!("read articles.json: {e}")))?;
    let mut articles: Vec<Article> = serde_json::from_str(&text)
        .map_err(|e| pocopine::ServerError::App(format!("parse articles.json: {e}")))?;

    // Append synthetic perf-test articles so users can exercise the
    // pipeline at real payload sizes without bloating articles.json.
    articles.push(synthetic_perf_article(
        "perf-50kb",
        "Performance test · 50 KB body",
        "2026-04-15",
        "A medium-sized body (~50 KB) for checking client-side parse and render times.",
        50_000,
    ));
    articles.push(synthetic_perf_article(
        "perf-500kb",
        "Performance test · 500 KB body",
        "2026-04-16",
        "A large body (~500 KB) — stresses JSON deserialisation, the fetch helper, and `pp-html` innerHTML replacement.",
        500_000,
    ));

    // Populate size metadata for every article (real + synthetic).
    for a in &mut articles {
        a.bytes = a.body.len();
        a.size_label = human_bytes(a.bytes);
    }

    Ok(articles)
}

#[cfg(not(target_arch = "wasm32"))]
fn synthetic_perf_article(
    slug: &str,
    title: &str,
    date: &str,
    excerpt: &str,
    approx_bytes: usize,
) -> Article {
    // Repeat a real-looking paragraph until we cross the target size.
    // `<p>` per repetition keeps `pp-html` rendering real paragraphs
    // rather than one monolithic blob.
    const PARA: &str =
        "<p>This paragraph is a deliberately repeated block used to size \
         the article's body to a measurable number of bytes. The whole \
         payload travels over the /_pocopine/get_article route as a JSON \
         <code>Result&lt;Article&gt;</code> and renders through <code>pp-html</code>.</p>";
    let mut body = String::with_capacity(approx_bytes + PARA.len());
    while body.len() < approx_bytes {
        body.push_str(PARA);
    }
    Article {
        slug: slug.into(),
        title: title.into(),
        date: date.into(),
        excerpt: excerpt.into(),
        body,
        bytes: 0,
        size_label: String::new(),
    }
}

#[pocopine::server]
pub async fn list_articles() -> ServerResult<Vec<ArticleSummary>> {
    let articles = load_articles_from_disk()?;
    Ok(articles.into_iter().map(ArticleSummary::from).collect())
}

#[pocopine::server]
pub async fn get_article(slug: String) -> ServerResult<Article> {
    let articles = load_articles_from_disk()?;
    articles
        .into_iter()
        .find(|a| a.slug == slug)
        .ok_or_else(|| pocopine::ServerError::App(format!("no article with slug: {slug}")))
}

#[pocopine::server]
pub async fn submit_contact(msg: ContactMessage) -> ServerResult<ContactResponse> {
    // Just log it. A real app would append to a DB or a queue.
    eprintln!(
        "contact: {name} <{email}>: {msg}",
        name = msg.name,
        email = msg.email,
        msg = msg.message,
    );
    // Tiny deterministic-ish id based on message length.
    let id = (msg.message.len() as u32).wrapping_add(1000);
    Ok(ContactResponse { id, ok: true })
}

// ─── components ──────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct AppShell {}

#[handlers]
impl AppShell {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Counter {
    pub count: i32,
}

#[handlers]
impl Counter {
    pub fn init(&mut self) {
        self.count = 0;
    }
    pub fn increment(&mut self) {
        self.count += 1;
    }
    pub fn decrement(&mut self) {
        self.count -= 1;
    }
    pub fn reset(&mut self) {
        self.count = 0;
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Home {}

#[handlers]
impl Home {}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Blog {
    pub loading: bool,
    pub error: String,
    pub list_html: String,
}

#[handlers]
impl Blog {
    pub fn init(&mut self) {
        self.loading = true;
        dispatch!(list_articles().await, |s, result| {
            s.loading = false;
            match result {
                Ok(articles) => {
                    s.error.clear();
                    s.list_html = render_article_list(&articles);
                }
                Err(e) => {
                    s.error = e.to_string();
                }
            }
        },);
    }
}

/// Build the article-list markup client-side. When it lands in the
/// DOM via `pp-html`, the MutationObserver walks the added nodes —
/// so `pp-route` on the anchors gets wired up like any other link.
fn render_article_list(articles: &[ArticleSummary]) -> String {
    if articles.is_empty() {
        return "<p><em>No articles yet.</em></p>".into();
    }
    let mut out = String::from("<ul class=\"article-list\">");
    for a in articles {
        out.push_str(&format!(
            "<li class=\"article-card\">\
                <div class=\"article-card__head\">\
                    <h3><a href=\"/blog/{slug}\" pp-route>{title}</a></h3>\
                    <span class=\"size-chip\" title=\"{bytes} bytes\">{size_label}</span>\
                </div>\
                <p class=\"article-date\">{date}</p>\
                <p>{excerpt}</p>\
            </li>",
            slug = html_escape(&a.slug),
            title = html_escape(&a.title),
            date = html_escape(&a.date),
            excerpt = html_escape(&a.excerpt),
            bytes = a.bytes,
            size_label = html_escape(&a.size_label),
        ));
    }
    out.push_str("</ul>");
    out
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

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct BlogPost {
    pub slug: String,
    pub title: String,
    pub date: String,
    pub body: String,
    pub size_label: String,
    pub bytes: u32,
    pub loading: bool,
    pub error: String,
    pub elapsed_ms: u32,
}

#[handlers]
impl BlogPost {
    pub fn init(&mut self) {
        self.loading = true;
        let slug = self.slug.clone();
        let start = performance_now();
        dispatch!(get_article(slug).await, |s, result| {
            s.loading = false;
            s.elapsed_ms = (performance_now() - start).round().max(0.0) as u32;
            match result {
                Ok(article) => {
                    s.title = article.title;
                    s.date = article.date;
                    s.body = article.body;
                    s.bytes = article.bytes as u32;
                    s.size_label = article.size_label;
                    s.error.clear();
                }
                Err(e) => {
                    s.error = e.to_string();
                }
            }
        },);
    }
}

/// Wall-clock now-ms for the perf counter. Falls back to 0 if the
/// Performance API isn't reachable (shouldn't happen in a browser).
fn performance_now() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Contact {
    pub name: String,
    pub email: String,
    pub message: String,
    pub submitting: bool,
    pub submitted: bool,
    pub last_id: u32,
    pub error: String,
}

#[handlers]
impl Contact {
    pub fn submit(&mut self) {
        if self.name.trim().is_empty()
            || self.email.trim().is_empty()
            || self.message.trim().is_empty()
        {
            self.error = "please fill out every field".into();
            return;
        }
        self.error.clear();
        self.submitting = true;
        let msg = ContactMessage {
            name: self.name.clone(),
            email: self.email.clone(),
            message: self.message.clone(),
        };
        dispatch!(submit_contact(msg).await, |s, result| {
            s.submitting = false;
            match result {
                Ok(resp) => {
                    s.submitted = true;
                    s.last_id = resp.id;
                    s.name.clear();
                    s.email.clear();
                    s.message.clear();
                }
                Err(e) => {
                    s.error = e.to_string();
                }
            }
        },);
    }
}

#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct NotFound {}

#[handlers]
impl NotFound {}

// ─── entry ────────────────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .register::<Counter>() // live demo on the home page
        .route::<Home>("/")
        .route::<Blog>("/blog")
        .route::<BlogPost>("/blog/:slug")
        .route::<Contact>("/contact")
        .route::<NotFound>("*")
        .run();
}
