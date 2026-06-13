//! `/blogs/*slug` — renders one blog post. Same machinery as
//! `DocPage`: the body is a static HTML fragment pre-rendered at build
//! time (from `docs/blogs/<slug>.md`) and **fetched** at mount
//! (`/static-docs/blogs/<slug>.html`), so the post does not bloat the
//! wasm bundle. Unlike the docs there is no sidebar — a single
//! centered article column with the post date above the title (the
//! title itself is the markdown `h1` inside the fragment).

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen_futures::spawn_local;

use crate::docs_data;

#[derive(Default, Serialize, Deserialize, RouteComponent)]
#[component(template = "BlogPage.poco", style = "blog_page.css", role = "panel")]
pub struct BlogPage {
    /// Rest param `*slug`, e.g. `pocopine-0-2-0`.
    #[prop]
    pub slug: String,
    pub html: String,
    pub date: String,
    pub has_date: bool,
    pub loading: bool,
    pub not_found: bool,
}

#[handlers]
impl BlogPage {
    pub fn on_setup(&mut self) {
        let slug = self.slug.clone();

        // Date comes from the small embedded BLOGS index.
        self.date = docs_data::BLOGS
            .iter()
            .find(|b| b.slug == slug)
            .map(|b| b.date_display.to_string())
            .unwrap_or_default();
        self.has_date = !self.date.is_empty();

        // Fetch the pre-rendered body fragment (served static file).
        self.loading = true;
        self.html.clear();
        self.not_found = false;
        let handle = this::<Self>();
        let url = format!("/static-docs/blogs/{slug}.html");
        spawn_local(async move {
            let fetched = crate::components::fetch_text(&url).await;
            let injected = handle.update(move |s: &mut BlogPage| {
                s.loading = false;
                match fetched {
                    Some(ref h) if !h.trim().is_empty() => {
                        s.html = h.clone();
                        s.not_found = false;
                        true
                    }
                    _ => {
                        s.not_found = true;
                        false
                    }
                }
            });
            if injected {
                // The `pp-html` effect runs in the reactive flush, which
                // is itself a queued microtask chain — a plain
                // `tick::next` here would fire *before* the innerHTML
                // write. `after_flush` (macrotask) is strictly later
                // than the whole flush. Plain document queries only —
                // no refs/scope lookups (scope context dies across
                // tick).
                pocopine::tick::after_flush(hydrate_videos);
            }
        });
    }

    /// Delegate clicks on internal links inside the injected markdown
    /// body so they route client-side.
    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        crate::components::delegate_nav(&ev);
    }
}

/// Kick muted autoplay on the `<video>` tags inside the injected post.
///
/// Chrome does not reliably reflect the `muted` *attribute* of a
/// fragment-parsed `<video>` (innerHTML via `pp-html`) onto the IDL
/// property, so the muted-autoplay policy silently blocks playback.
/// Re-assert the properties imperatively and call `play()`; the
/// returned promise (and any policy rejection) is ignored — the tags
/// carry `controls` as the user-visible fallback.
fn hydrate_videos() {
    use wasm_bindgen::{JsCast, JsValue};

    let Some(doc) = web_sys::window().and_then(|w| w.document()) else {
        return;
    };
    let Ok(Some(article)) = doc.query_selector(".blog-layout article.doc-body") else {
        return;
    };
    let Ok(videos) = article.query_selector_all("video") else {
        return;
    };
    for i in 0..videos.length() {
        let Some(node) = videos.item(i) else {
            continue;
        };
        let video = JsValue::from(node);
        let _ = js_sys::Reflect::set(&video, &"muted".into(), &true.into());
        let _ = js_sys::Reflect::set(&video, &"playsInline".into(), &true.into());
        let Ok(play) = js_sys::Reflect::get(&video, &"play".into()) else {
            continue;
        };
        if let Ok(play) = play.dyn_into::<js_sys::Function>() {
            let _ = play.call0(&video);
        }
    }
}
