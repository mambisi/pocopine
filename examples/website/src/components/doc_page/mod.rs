//! `/docs/*slug` — renders a documentation page. The page body is a
//! static HTML fragment pre-rendered at build time and **fetched** at
//! mount (`/static-docs/<slug>.html`), so the markdown content does
//! not bloat the wasm bundle. Only the small nav + table-of-contents
//! metadata is embedded (`crate::docs_data`). Three-column layout:
//! `site.toml`-driven sidebar, injected content, on-this-page TOC.
//! The router repaints on every URL change, so `on_mount` re-derives
//! everything from the `slug` and kicks off the fetch.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen_futures::spawn_local;

use crate::docs_data;

/// One sidebar row — a group header or a page link. The nav is
/// flattened to a single list (pocopine nested `pp-for` does not
/// iterate an inner loop over an outer loop variable's field).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct NavRow {
    pub is_header: bool,
    pub label: String,
    pub href: String,
    pub active: bool,
    pub key: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct TocView {
    pub text: String,
    pub href: String,
    pub sub: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "DocPage.poco", style = "doc_page.css", role = "panel")]
pub struct DocPage {
    /// Rest param `*slug`, e.g. `guides/auth/credentials`.
    #[prop]
    pub slug: String,
    pub html: String,
    pub title: String,
    pub loading: bool,
    pub not_found: bool,
    pub nav: Vec<NavRow>,
    pub toc: Vec<TocView>,
    pub has_toc: bool,
}

#[handlers]
impl DocPage {
    // on_setup (pre-render) so the sidebar/TOC `pp-for`s have data when
    // the compiled template installs clone directives; the body HTML is
    // fetched async and injected via pp-html once it arrives.
    pub fn on_setup(&mut self) {
        let slug = if self.slug.is_empty() {
            "getting-started/introduction".to_string()
        } else {
            self.slug.clone()
        };

        // Sidebar + TOC + title come from the small embedded metadata.
        self.title = docs_data::page_title(&slug).to_string();
        self.toc = docs_data::page_toc(&slug)
            .iter()
            .map(|t| TocView {
                text: t.text.into(),
                href: format!("#{}", t.id),
                sub: t.depth >= 3,
            })
            .collect();
        self.has_toc = !self.toc.is_empty();

        // Flatten NAV into header + link rows (a group header precedes
        // each group's pages).
        let mut rows: Vec<NavRow> = Vec::new();
        let mut last_group = "";
        for item in docs_data::NAV {
            if item.group != last_group {
                rows.push(NavRow {
                    is_header: true,
                    label: item.group.into(),
                    href: String::new(),
                    active: false,
                    key: format!("g:{}", item.group),
                });
                last_group = item.group;
            }
            let href = format!("/docs/{}", item.slug);
            rows.push(NavRow {
                is_header: false,
                label: item.title.into(),
                active: item.slug == slug,
                key: href.clone(),
                href,
            });
        }
        self.nav = rows;

        // Fetch the pre-rendered body fragment (served static file).
        self.loading = true;
        self.html.clear();
        self.not_found = false;
        let handle = this::<Self>();
        let url = format!("/static-docs/{slug}.html");
        spawn_local(async move {
            let fetched = fetch_text(&url).await;
            handle.update(move |s: &mut DocPage| {
                s.loading = false;
                match fetched {
                    Some(ref h) if !h.trim().is_empty() => {
                        s.html = h.clone();
                        s.not_found = false;
                    }
                    _ => s.not_found = true,
                }
            });
        });
    }

    /// Delegate clicks on internal links — both the sidebar and the
    /// links inside the injected markdown body route client-side.
    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        crate::components::delegate_nav(&ev);
    }
}

impl RouteComponent for DocPage {}

/// Fetch a text resource (the pre-rendered doc fragment) via the
/// browser `fetch` API. Returns `None` on any network/4xx failure.
async fn fetch_text(url: &str) -> Option<String> {
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;

    let win = web_sys::window()?;
    let resp = JsFuture::from(win.fetch_with_str(url)).await.ok()?;
    let resp: web_sys::Response = resp.dyn_into().ok()?;
    if !resp.ok() {
        return None;
    }
    let text = JsFuture::from(resp.text().ok()?).await.ok()?;
    text.as_string()
}
