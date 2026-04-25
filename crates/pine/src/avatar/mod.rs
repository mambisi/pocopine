//! `<pine-avatar-*>` — image-with-fallback primitive.
//!
//! Compound after reka-ui's Avatar. Three parts:
//!
//! - **Root** (`pine-avatar-root`) — holds `loaded: bool`,
//!   provides the Handle so Image and Fallback can subscribe.
//! - **Image** (`pine-avatar-image`) — `<img>` that flips Root's
//!   `loaded` to `true` on the browser's `load` event, stays
//!   hidden via `pp-show` until then.
//! - **Fallback** (`pine-avatar-fallback`) — gated via
//!   `pp-show="!loaded"` so it's visible during load + on error.
//!
//! ```html
//! <pine-avatar-root>
//!   <pine-avatar-image src="https://example.com/a.jpg" alt="A"></pine-avatar-image>
//!   <pine-avatar-fallback>AB</pine-avatar-fallback>
//! </pine-avatar-root>
//! ```

use pocopine::create_context;
use pocopine::prelude::*;
use serde::{Deserialize, Serialize};

create_context!(ROOT: Handle<PineAvatarRoot>);

// ── Root ──────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAvatarRoot.poco", role = "visual")]
// RFC 049 — an Avatar wraps exactly Image + Fallback. The
// Image loads; when load fails the Fallback renders in its
// place. Any other direct child breaks the load/fail swap.
#[slot(default, only = [PineAvatarImage, PineAvatarFallback])]
pub struct PineAvatarRoot {
    pub loaded: bool,
}

#[handlers]
impl PineAvatarRoot {
    fn on_setup(&mut self) {
        ROOT.provide(this::<Self>());
    }
}

// ── Image ─────────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAvatarImage.poco", role = "visual")]
pub struct PineAvatarImage {
    #[prop]
    pub src: String,
    #[prop]
    pub alt: String,
    /// Mirrored from Root. Drives `pp-show` so the browser's
    /// broken-image glyph never flashes during load.
    #[observe(ROOT)]
    pub loaded: bool,
}

#[handlers]
impl PineAvatarImage {
    /// `@load` on the img — fires once the browser has the
    /// bitmap. Promotes Root to loaded so Fallback hides.
    pub fn on_load(&mut self) {
        if let Some(root) = ROOT.inject() {
            root.update(|r: &mut PineAvatarRoot| r.loaded = true);
        }
    }

    /// `@error` on the img — broken URL or network error. Leave
    /// Root.loaded = false so Fallback stays visible. No state
    /// update needed; included so authors can observe failures
    /// via `@error` on the tag itself.
    pub fn on_error(&mut self) {}
}

// ── Fallback ──────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
#[component(template = "PineAvatarFallback.poco", role = "visual")]
pub struct PineAvatarFallback {
    /// Mirrored from Root. Fallback's `pp-if="!loaded"` reads it
    /// — present during load, unmounted once the image lands.
    #[observe(ROOT)]
    pub loaded: bool,
}

#[handlers]
impl PineAvatarFallback {}
