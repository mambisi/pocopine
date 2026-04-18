//! pocopine — the marketing site. Client-side SPA + server functions.

pub mod shared;

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use shared::{human_bytes, ArticleSummary, ContactMessage, ContactResponse};
#[cfg(not(target_arch = "wasm32"))]
use shared::Article;

// ─── server functions ────────────────────────────────────────────

/// Host-side helper — reads articles.json off disk. Shared by the two
/// server fns below so they parse once. Target-gated because the
/// client stub never executes this body.
/// `articles.json` is embedded into the server binary at compile time
/// via `include_str!`. No runtime disk I/O; edits require a rebuild.
#[cfg(not(target_arch = "wasm32"))]
const ARTICLES_JSON: &str = include_str!("../articles.json");

/// Load every article — the real ones from `ARTICLES_JSON`, the two
/// big perf fixtures, and a pool of 500 small synthetic articles for
/// infinite-scroll testing. Shared by every server fn so parsing and
/// generation happens once per request.
#[cfg(not(target_arch = "wasm32"))]
fn load_articles() -> Result<Vec<Article>, pocopine::ServerError> {
    let mut articles: Vec<Article> = serde_json::from_str(ARTICLES_JSON)
        .map_err(|e| pocopine::ServerError::App(format!("parse articles.json: {e}")))?;

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

    // Pool of 500 tiny articles — feeds the /perf infinite-scroll
    // demo with enough rows to actually exercise pagination.
    for i in 1..=500_u32 {
        let month = ((i - 1) / 30 % 12) + 1;
        let day = ((i - 1) % 30) + 1;
        articles.push(Article {
            slug: format!("article-{i:04}"),
            title: format!("Generated article #{i}"),
            date: format!("2026-{month:02}-{day:02}"),
            excerpt: format!(
                "Synthetic article #{i} — used for paginated fetch + list-render \
                 benchmarks on the /perf page."
            ),
            body: format!(
                "<p>Body of generated article #{i}.</p>\
                 <p>This pool is intentionally short per row. It's here to \
                 measure client-side list rendering and the server-fn \
                 pagination pipeline, not individual page payload size.</p>"
            ),
            bytes: 0,
            size_label: String::new(),
        });
    }

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
pub async fn list_articles_page(offset: u32, limit: u32) -> ServerResult<Vec<ArticleSummary>> {
    let articles = load_articles()?;
    let total = articles.len();
    let start = (offset as usize).min(total);
    let end = start.saturating_add(limit as usize).min(total);
    Ok(articles[start..end]
        .iter()
        .cloned()
        .map(ArticleSummary::from)
        .collect())
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

/// Perf page — paginated infinite-scroll + a counter at the top
/// demonstrating that reactivity reruns only the effects subscribed
/// to a changed key (the list doesn't re-render when the counter
/// increments).
#[derive(Default, Serialize, Deserialize)]
#[component]
pub struct Perf {
    pub loaded: u32,
    pub page: u32,
    pub last_ms: u32,
    pub total_bytes: u32,
    pub total_bytes_label: String,
    pub list_html: String,
    pub loading: bool,
    pub error: String,
    pub exhausted: bool,
}

const PAGE_SIZE: u32 = 20;

#[handlers]
impl Perf {
    pub fn init(&mut self) {
        if self.loaded == 0 {
            self.load_more();
        }
        // Bind an IntersectionObserver to the bottom sentinel so the
        // next page fetches as soon as it scrolls into view — no
        // button, no click. The observer re-enters through
        // `Scope::invoke("load_more", ...)` so it hits the same
        // handler path a button click would.
        if let Some(scope_id) = pocopine::current_scope_id() {
            install_scroll_observer(scope_id);
        }
    }

    pub fn load_more(&mut self) {
        if self.loading || self.exhausted {
            return;
        }
        self.loading = true;
        let offset = self.loaded;
        let start = performance_now();
        dispatch!(
            list_articles_page(offset, PAGE_SIZE).await,
            |s, result| {
                s.loading = false;
                s.last_ms = (performance_now() - start).round().max(0.0) as u32;
                match result {
                    Ok(batch) => {
                        if batch.is_empty() {
                            s.exhausted = true;
                            return;
                        }
                        s.page += 1;
                        for a in &batch {
                            s.total_bytes += a.bytes as u32;
                        }
                        s.total_bytes_label = human_bytes(s.total_bytes as usize);
                        s.loaded += batch.len() as u32;
                        s.list_html.push_str(&render_batch(&batch));
                        s.error.clear();
                    }
                    Err(e) => {
                        s.error = e.to_string();
                    }
                }
            },
        );
    }
}

/// Install an `IntersectionObserver` that watches the `.perf-sentinel`
/// element and invokes `load_more` on `scope_id` each time the
/// sentinel scrolls into view. `load_more` itself no-ops when a fetch
/// is already in flight or the pool is exhausted, so re-entry during
/// a single intersection event is safe.
fn install_scroll_observer(scope_id: pocopine::ScopeId) {
    use std::cell::Cell;
    thread_local! { static INSTALLED: Cell<bool> = const { Cell::new(false) }; }
    if INSTALLED.with(|c| c.replace(true)) {
        return;
    }
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let Ok(Some(sentinel)) = doc.query_selector(".perf-sentinel") else { return };

    let cb = wasm_bindgen::closure::Closure::wrap(Box::new(
        move |entries: js_sys::Array, _obs: wasm_bindgen::JsValue| {
            for i in 0..entries.length() {
                let entry = entries.get(i);
                let intersecting = js_sys::Reflect::get(
                    &entry,
                    &wasm_bindgen::JsValue::from_str("isIntersecting"),
                )
                .ok()
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
                if intersecting {
                    if let Some(scope) = pocopine::Scope::find(scope_id) {
                        scope.invoke("load_more", &js_sys::Array::new());
                    }
                }
            }
        },
    )
        as Box<dyn FnMut(js_sys::Array, wasm_bindgen::JsValue)>);

    use wasm_bindgen::JsCast;
    let Ok(observer) = web_sys::IntersectionObserver::new(cb.as_ref().unchecked_ref()) else {
        return;
    };
    observer.observe(&sentinel);
    cb.forget();
}

fn render_batch(articles: &[ArticleSummary]) -> String {
    let mut out = String::new();
    for a in articles {
        out.push_str(&format!(
            "<li class=\"article-card\">\
                <div class=\"article-card__head\">\
                    <h3>{title}</h3>\
                    <span class=\"size-chip\">{size_label}</span>\
                </div>\
                <p class=\"article-date\">{date}</p>\
                <p>{excerpt}</p>\
            </li>",
            title = html_escape(&a.title),
            date = html_escape(&a.date),
            size_label = html_escape(&a.size_label),
            excerpt = html_escape(&a.excerpt),
        ));
    }
    out
}

// ─── entry ────────────────────────────────────────────────────────

#[wasm_bindgen(start)]
pub fn main() {
    App::new()
        .register::<AppShell>()
        .register::<Counter>() // live demo on the home page and /perf
        .route::<Home>("/")
        .route::<Contact>("/contact")
        .route::<Perf>("/perf")
        .route::<NotFound>("*")
        .run();
}
