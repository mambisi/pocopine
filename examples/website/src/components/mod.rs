//! Page components. Each subdirectory holds one `#[component]`
//! plus its sibling `.poco` template — the same layout the `hn`
//! example uses.

pub mod blog_page;
pub mod blogs_index;
pub mod charts_page;
pub mod component_meta;
pub mod components_index;
pub mod components_page;
pub mod doc_page;
pub mod easing_playground;
pub mod hero;
pub mod icons_page;
pub mod install_cmd;
pub mod issue_flow_demo;
pub mod landing;
pub mod motion_page;
pub mod secure_section;
pub mod showcase;
pub mod showcase_card;
pub mod site_header;
pub mod stack_flow;
pub mod stack_showcase;
pub mod tutorial;

pub use blog_page::BlogPage;
pub use blogs_index::BlogsIndex;
pub use charts_page::ChartsPage;
pub use components_index::ComponentsIndex;
pub use components_page::ComponentPage;
pub use doc_page::DocPage;
pub use easing_playground::EasingPlayground;
pub use hero::Hero;
pub use icons_page::IconsPage;
pub use install_cmd::InstallCmd;
pub use issue_flow_demo::IssueFlowDemo;
pub use landing::Landing;
pub use motion_page::MotionPage;
pub use secure_section::SecureSection;
pub use showcase::Showcase;
pub use showcase_card::ShowcaseCard;
pub use site_header::SiteHeader;
pub use stack_flow::StackFlow;
pub use stack_showcase::StackShowcase;
pub use tutorial::Tutorial;

/// Fetch a text resource (a pre-rendered docs/blog fragment) via the
/// browser `fetch` API. Returns `None` on any network/4xx failure.
pub async fn fetch_text(url: &str) -> Option<String> {
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

/// RFC-099 — read a pre-rendered static fragment off disk during SSR so
/// its content lands in the first paint (byte-equal on a route refresh,
/// no fetch flicker). `rel` is the path under the served static root —
/// e.g. `static-docs/<slug>.html` — the exact file the client fetches and
/// the server serves; the root matches the server bin's resolution
/// (`POCOPINE_DIST`, else the crate dir). Returns `None` on a path-
/// traversal attempt or a missing/empty file (caller renders not-found).
#[cfg(not(target_arch = "wasm32"))]
pub fn read_static_fragment(rel: &str) -> Option<String> {
    if rel.contains("..") {
        return None;
    }
    let root =
        std::env::var("POCOPINE_DIST").unwrap_or_else(|_| env!("CARGO_MANIFEST_DIR").to_string());
    let html = std::fs::read_to_string(std::path::Path::new(&root).join(rel)).ok()?;
    (!html.trim().is_empty()).then_some(html)
}

/// Shared SPA-navigation click delegate.
///
/// `pp-route` cannot be used inside a `<template pp-for>` clone — it
/// makes the compiled clone body render as raw HTML (directives never
/// install). Instead, list components put `pp-on:click` on their root
/// and call this: it finds the clicked internal `<a href="/…">` and
/// routes it client-side, leaving external / `#` links alone.
pub fn delegate_nav(ev: &web_sys::MouseEvent) {
    use wasm_bindgen::JsCast;
    let Some(el) = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
    else {
        return;
    };
    // Prefer an enclosing real link's `href`; fall back to a `[data-href]`
    // card. Cards that wrap a live interactive preview can't be an `<a>`
    // (a nested `<a>` inside the preview is invalid HTML and the browser
    // splits it when the SSR'd page is parsed) — they're a `<div
    // data-href>` instead, navigated by this same delegated click.
    let href = el
        .closest("a[href]")
        .ok()
        .flatten()
        .and_then(|a| a.get_attribute("href"))
        .or_else(|| {
            el.closest("[data-href]")
                .ok()
                .flatten()
                .and_then(|d| d.get_attribute("data-href"))
        });
    if let Some(href) = href {
        if href.starts_with('/') {
            ev.prevent_default();
            pocopine::navigate(&href);
        }
    }
}

/// Generic 4-column row for the hand-authored API tables on the section
/// pages, rendered with the global `.props-table` styling. The columns
/// mean different things per table (see each page's `<thead>`); the first
/// three render as `<code>`, the last as prose.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Row4 {
    pub key: String,
    pub a: String,
    pub b: String,
    pub c: String,
    pub d: String,
}

impl Row4 {
    pub fn new(a: &str, b: &str, c: &str, d: &str) -> Self {
        Row4 {
            key: format!("{a}|{b}"),
            a: a.into(),
            b: b.into(),
            c: c.into(),
            d: d.into(),
        }
    }
}

/// Generic 2-column API-table row (e.g. signature → what it does). The
/// first column renders as `<code>`, the second as prose.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Row2 {
    pub key: String,
    pub a: String,
    pub b: String,
}

impl Row2 {
    pub fn new(a: &str, b: &str) -> Self {
        Row2 {
            key: a.into(),
            a: a.into(),
            b: b.into(),
        }
    }
}
