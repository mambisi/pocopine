//! Sticky top bar: thin view-only component. Palette state, theme,
//! and every action live on `WebsiteApp` (the app-shell provider).
//! The header injects the root handle to forward clicks, and
//! `#[observe]` mirrors the root's `theme` so the sun/moon icon
//! updates reactively.

use pocopine::inject;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{WebsiteApp, WEBSITE_APP};

#[derive(Default, Serialize, Deserialize)]
#[component(
    template = "SiteHeader.poco",
    style = "site_header.css",
    role = "panel"
)]
pub struct SiteHeader {
    #[observe(crate::WEBSITE_APP)]
    pub theme: String,
}

#[handlers]
impl SiteHeader {
    pub fn open_palette(&mut self) {
        if let Some(app) = inject::<Handle<WebsiteApp>>(&WEBSITE_APP) {
            app.update(|a: &mut WebsiteApp| a.open_palette());
        }
    }

    pub fn toggle_theme(&mut self) {
        if let Some(app) = inject::<Handle<WebsiteApp>>(&WEBSITE_APP) {
            app.update(|a: &mut WebsiteApp| a.toggle_theme());
        }
    }
}
