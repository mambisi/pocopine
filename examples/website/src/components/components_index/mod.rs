//! `/components` — the component reference index. Renders the
//! `component_meta::COMPONENTS` table as category grids of cards
//! linking to each `/components/:slug` page.
//!
//! Categories are fixed (5), so each is its own field with a single
//! `pp-for` — pocopine's nested `pp-for` (inner loop over an outer
//! loop variable's field) does not iterate, so we flatten instead.

use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::component_meta::{
    CAT_DATETIME, CAT_DISPLAY, CAT_FORMS, CAT_NAV, CAT_OVERLAYS, COMPONENTS,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct CardView {
    pub title: String,
    pub blurb: String,
    pub href: String,
}

#[derive(Default, Serialize, Deserialize)]
#[component(template = "ComponentsIndex.poco", style = "components_index.css")]
pub struct ComponentsIndex {
    pub forms: Vec<CardView>,
    pub overlays: Vec<CardView>,
    pub navigation: Vec<CardView>,
    pub display: Vec<CardView>,
    pub datetime: Vec<CardView>,
}

fn cards(category: &str) -> Vec<CardView> {
    COMPONENTS
        .iter()
        .filter(|c| c.category == category)
        .map(|c| CardView {
            title: c.title.into(),
            blurb: c.blurb.into(),
            href: format!("/components/{}", c.slug),
        })
        .collect()
}

#[handlers]
impl ComponentsIndex {
    // on_setup (pre-render) so each category `pp-for` has its data
    // when the compiled template installs clone directives.
    pub fn on_setup(&mut self) {
        self.forms = cards(CAT_FORMS);
        self.overlays = cards(CAT_OVERLAYS);
        self.navigation = cards(CAT_NAV);
        self.display = cards(CAT_DISPLAY);
        self.datetime = cards(CAT_DATETIME);
    }

    pub fn on_nav(&mut self, ev: web_sys::MouseEvent) {
        crate::components::delegate_nav(&ev);
    }
}

impl RouteComponent for ComponentsIndex {}
