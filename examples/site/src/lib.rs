//! pocopine — the marketing site. Client-side SPA + server functions.

pub mod shared;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use shared::{Article, ArticleSummary, ContactMessage, ContactResponse};

// ─── server functions ────────────────────────────────────────────

/// Host-side helper — reads articles.json off disk. Shared by the two
/// server fns below so they parse once. Target-gated because the
/// client stub never executes this body.
#[cfg(not(target_arch = "wasm32"))]
fn load_articles_from_disk() -> Result<Vec<Article>, pocopine::ServerError> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/articles.json");
    let text = std::fs::read_to_string(path)
        .map_err(|e| pocopine::ServerError::App(format!("read articles.json: {e}")))?;
    serde_json::from_str::<Vec<Article>>(&text)
        .map_err(|e| pocopine::ServerError::App(format!("parse articles.json: {e}")))
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
                <h3><a href=\"/blog/{slug}\" pp-route>{title}</a></h3>\
                <p class=\"article-date\">{date}</p>\
                <p>{excerpt}</p>\
            </li>",
            slug = html_escape(&a.slug),
            title = html_escape(&a.title),
            date = html_escape(&a.date),
            excerpt = html_escape(&a.excerpt),
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
    pub loading: bool,
    pub error: String,
}

#[handlers]
impl BlogPost {
    pub fn init(&mut self) {
        self.loading = true;
        let slug = self.slug.clone();
        dispatch!(get_article(slug).await, |s, result| {
            s.loading = false;
            match result {
                Ok(article) => {
                    s.title = article.title;
                    s.date = article.date;
                    s.body = article.body;
                    s.error.clear();
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
