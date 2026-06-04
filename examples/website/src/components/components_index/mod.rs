//! `/components` — the component reference home. A curated 3-column
//! showcase of a few favourite primitives, each a live preview that
//! links to its page. The full catalogue lives in the left sidebar on
//! every component page (see `components_page`), so this stays a
//! friendly entry point rather than an exhaustive list.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ComponentsIndex.poco", style = "components_index.css")]
pub struct ComponentsIndex {}

#[handlers]
impl ComponentsIndex {
    /// Delegated SPA nav — each card is a plain `<a href="/components/…">`
    /// whose click bubbles here (the live preview inside is
    /// `pointer-events: none`, so the card itself takes the click).
    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        crate::components::delegate_nav(&ev);
    }
}

impl RouteComponent for ComponentsIndex {}
