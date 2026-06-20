//! `/blogs/*slug` — renders one blog post. Same machinery as
//! `DocPage`: the body is a static HTML fragment pre-rendered at build
//! time (from `docs/blogs/<slug>.md`) and **fetched** at mount
//! (`/static-docs/blogs/<slug>.html`), so the post does not bloat the
//! wasm bundle. Unlike the docs there is no sidebar — a single
//! centered article column with the post date above the title (the
//! title itself is the markdown `h1` inside the fragment).

use pocopine::prelude::*;
#[cfg(target_arch = "wasm32")]
use pocopine::spawn_local;
use serde::{Deserialize, Serialize};

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

        // The pre-rendered body fragment is served as
        // /static-docs/blogs/<slug>.html and kept OUT of the wasm bundle.
        self.not_found = false;
        let rel = format!("static-docs/blogs/{slug}.html");

        #[cfg(not(target_arch = "wasm32"))]
        {
            // SSR: read it off disk so the post body is in the first paint
            // — byte-equal on a refresh, no fetch flicker. (Video hydration
            // is a client concern, handled below on the hydration pass.)
            match crate::components::read_static_fragment(&rel) {
                Some(html) => {
                    self.html = html;
                    self.loading = false;
                }
                None => {
                    self.loading = false;
                    self.not_found = true;
                }
            }
        }

        #[cfg(target_arch = "wasm32")]
        {
            if self.html.trim().is_empty() {
                // No SSR body (client navigation) — fetch the fragment.
                self.loading = true;
                let handle = this::<Self>();
                let url = format!("/{rel}");
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
                        // The `pp-html` effect runs in the reactive flush
                        // (a queued microtask chain); `after_flush` (a
                        // macrotask) is strictly later than the whole
                        // flush, so the innerHTML is written by then.
                        pocopine::tick::after_flush(hydrate_videos);
                    }
                });
            } else {
                // Hydrated from the SSR island — body already present; the
                // claim self-heals it byte-equal (no fetch, no flash), but
                // its videos still need wiring up on the client.
                self.loading = false;
                pocopine::tick::after_flush(hydrate_videos);
            }
        }
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
/// Re-assert the properties imperatively and call `play()`; any
/// rejection of the returned promise is swallowed with a no-op
/// `catch` — the tags carry `controls` as the user-visible fallback.
#[cfg(target_arch = "wasm32")]
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
    if videos.length() == 0 {
        return;
    }
    // `play()` returns a promise that *rejects* when the play request
    // is interrupted (a pause()/load()/source-change racing the async
    // playback start) — see
    // https://developer.chrome.com/blog/play-request-was-interrupted
    // Dropping that rejection logs an unhandled-rejection error, so
    // attach a no-op catch. One shared closure, forgotten once per
    // page view — a tiny deliberate leak.
    let noop = wasm_bindgen::closure::Closure::new(|_: JsValue| {});
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
            let Ok(ret) = play.call0(&video) else {
                continue;
            };
            if let Ok(promise) = ret.dyn_into::<js_sys::Promise>() {
                let _ = promise.catch(&noop);
            }
        }
    }
    noop.forget();
}
