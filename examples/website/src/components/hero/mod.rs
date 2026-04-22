//! Landing hero — mascot, wordmark, tagline, CTAs.
//!
//! Demonstrates the pocopine **root-provider** pattern: the
//! "Get started" CTA reaches up to the app root (`WebsiteApp`),
//! injected by key, and tells it to open the site-wide command
//! palette. `WebsiteApp` is the canonical provider for anything
//! app-global (palette, theme) because it encloses every other
//! component's scope — no prop drilling, no shared store.

use pocopine::prelude::*;
use pocopine::inject;
use serde::{Deserialize, Serialize};

use crate::{WebsiteApp, WEBSITE_APP};

#[derive(Default, Serialize, Deserialize)]
#[component(template = "Hero.poco", style = "hero.css", role = "panel")]
pub struct Hero {}

#[handlers]
impl Hero {
    pub fn open_site_palette(&mut self) {
        if let Some(app) = inject::<Handle<WebsiteApp>>(&WEBSITE_APP) {
            app.update(|a: &mut WebsiteApp| a.open_palette());
        }
    }
}
